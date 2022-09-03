use fbas_analyzer::*;
use fbas_reward_distributor::*;

use env_logger::Env;
use log::{debug, info};
use par_map::ParMap;
use std::{collections::BTreeMap, error::Error, io, path::PathBuf};
use structopt::StructOpt;

/// Run performance measurements on different sized FBASs based on the input parameters.
#[derive(Debug, StructOpt)]
#[structopt(
    name = "performance_tests",
    about = "Run performance measurements on different sized FBASs based on the input parameters
",
    author = "Charmaine Ndolo"
)]
struct Cli {
    /// Output CSV file (will output to STDOUT if omitted).
    #[structopt(short = "o", long = "out")]
    output_path: Option<PathBuf>,

    /// Largest FBAS to analyze, measured in number of top-tier nodes.
    #[structopt(short = "m", long = "max-top-tier-size")]
    max_top_tier_size: usize,

    // necessary to nest because two subcommands are not allowed
    #[structopt(flatten)]
    run_config: RunConfig,

    /// Update output file with missing results (doesn't repeat analyses for existing lines).
    #[structopt(short = "u", long = "update")]
    update: bool,

    /// Number of analysis runs per FBAS size.
    #[structopt(short = "r", long = "runs", default_value = "10")]
    runs: usize,

    /// Number of threads to use. Defaults to 1.
    #[structopt(short = "j", long = "jobs", default_value = "1")]
    jobs: usize,

    /// Do not assert that the FBAS has quorum intersection before proceeding with further computations.
    /// Default behaviour is to always check for QI.
    #[structopt(long = "no-quorum-intersection")]
    dont_check_for_qi: bool,

    /// Monitor system's memory usage
    #[structopt(short = "s", long = "system")]
    sysinfo: bool,
}

#[derive(Debug, StructOpt)]
// weird workaround because two subcommands are not allowed
struct RunConfig {
    #[structopt(subcommand)]
    ranking_alg: RankingAlgConfig,
    fbas_type: FbasType,
}

#[derive(Debug, StructOpt)]
enum RankingAlgConfig {
    /// Use NodeRank, an extension of PageRank, to measure nodes' weight in the FBAS
    NodeRank,
    /// Use Shapley-Shubik power indices to calculate nodes' importance in the FBAS. Not
    /// recommended for FBAS with many players because of time complexity
    PowerIndexEnum,
    /// Approximate Shapley values as a measure of nodes' importance in the FBAS.
    /// Number of samples to use for the approximation must be passed.
    PowerIndexApprox { s: usize },
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Cli::from_args();
    let env = Env::default()
        .filter_or("LOG_LEVEL", "info")
        .write_style_or("LOG_STYLE", "always");
    env_logger::init_from_env(env);
    let fbas_type = args.run_config.fbas_type;
    let ranking_alg = match args.run_config.ranking_alg {
        RankingAlgConfig::NodeRank => RankingAlg::NodeRank,
        RankingAlgConfig::PowerIndexEnum => RankingAlg::PowerIndexEnum(None),
        RankingAlgConfig::PowerIndexApprox { s } => RankingAlg::PowerIndexApprox(s),
    };
    let inputs: Vec<InputDataPoint> =
        generate_inputs(args.max_top_tier_size, args.runs, fbas_type.clone());
    let existing_outputs = if args.update {
        load_existing_outputs(&args.output_path)?
    } else {
        BTreeMap::new()
    };
    let tasks = make_sorted_tasklist(inputs, existing_outputs);

    let qi_check = !args.dont_check_for_qi;
    let mem_monitor = args.sysinfo;
    let output_iterator = bulk_do(
        tasks,
        args.jobs,
        fbas_type.clone(),
        qi_check,
        ranking_alg,
        mem_monitor,
    );
    println!("Starting performance measurements for {:?} like FBAS with upto {} nodes.\n Performing {} iterations per FBAS.",fbas_type, args.max_top_tier_size, args.runs);

    write_csv(output_iterator, &args.output_path, args.update)?;
    Ok(())
}

fn generate_inputs(
    max_top_tier_size: usize,
    runs: usize,
    fbas_type: FbasType,
) -> Vec<InputDataPoint> {
    let mut inputs = vec![];
    for top_tier_size in (1..max_top_tier_size + 1).filter(|m| m % fbas_type.node_increments() == 0)
    {
        for run in 0..runs {
            inputs.push(InputDataPoint { top_tier_size, run });
        }
    }
    inputs
}

fn load_existing_outputs(
    path: &Option<PathBuf>,
) -> Result<BTreeMap<InputDataPoint, PerfDataPoint>, Box<dyn Error>> {
    if let Some(path) = path {
        let data_points = read_csv_from_file(path)?;
        let data_points_map = data_points
            .into_iter()
            .map(|d| (InputDataPoint::from_perf_data_point(&d), d))
            .collect();
        Ok(data_points_map)
    } else {
        Ok(BTreeMap::new())
    }
}

fn make_sorted_tasklist(
    inputs: Vec<InputDataPoint>,
    existing_outputs: BTreeMap<InputDataPoint, PerfDataPoint>,
) -> Vec<Task> {
    let mut tasks: Vec<Task> = inputs
        .into_iter()
        .filter_map(|input| {
            if !existing_outputs.contains_key(&input) {
                Some(Task::Analyze(input))
            } else {
                None
            }
        })
        .chain(existing_outputs.values().cloned().map(Task::ReusePerfData))
        .collect();
    tasks.sort_by_cached_key(|t| t.label());
    tasks
}

fn bulk_do(
    tasks: Vec<Task>,
    jobs: usize,
    fbas_type: FbasType,
    qi_check: bool,
    alg: RankingAlg,
    monitor: bool,
) -> impl Iterator<Item = PerfDataPoint> {
    tasks
        .into_iter()
        .with_nb_threads(jobs)
        .par_map(move |task| {
            analyze_or_reuse(task, fbas_type.clone(), qi_check, alg.clone(), monitor)
        })
}

fn analyze_or_reuse(
    task: Task,
    fbas_type: FbasType,
    qi_check: bool,
    alg: RankingAlg,
    monitor: bool,
) -> PerfDataPoint {
    match task {
        Task::ReusePerfData(output) => {
            eprintln!(
                "Reusing existing analysis results for m={}, run={}.",
                output.top_tier_size, output.run
            );
            output
        }
        Task::Analyze(input) => batch_rank(input, fbas_type, qi_check, alg, monitor),
        _ => panic!("Unexpected data point"),
    }
}

fn rank_fbas(
    input: InputDataPoint,
    fbas: &Fbas,
    alg: RankingAlg,
    qi_check: bool,
    monitor: bool,
) -> (f64, SysInfo) {
    let size = fbas.number_of_nodes();
    info!(
        "Starting {:?} run {} for FBAS of size {}.",
        alg, input.run, size
    );
    let (tot_mem, used_mem, avail_mem, free_mem, tot_swap, used_swap) = if monitor {
        let mut system = fbas_reward_distributor::stats::init_sysinfo_instance();
        get_system_mem_info(&mut system)
    } else {
        (
            u64::default(),
            u64::default(),
            u64::default(),
            u64::default(),
            u64::default(),
            u64::default(),
        )
    };
    let (_, duration) = timed_secs!(rank_nodes(fbas, alg.clone(), qi_check));
    debug!(
        "Completed {:?} run {} for FBAS of size {}.",
        alg, input.run, size
    );
    (
        duration,
        (tot_mem, used_mem, avail_mem, free_mem, tot_swap, used_swap),
    )
}

fn batch_rank(
    input: InputDataPoint,
    fbas_type: FbasType,
    qi_check: bool,
    alg: RankingAlg,
    monitor: bool,
) -> PerfDataPoint {
    let fbas = fbas_type.make_one(input.top_tier_size);
    assert!(fbas.number_of_nodes() == input.top_tier_size);
    let size = fbas.number_of_nodes();
    info!("Starting run {} for FBAS with {} nodes", input.run, size);

    let (duration, mem_info) = if alg == RankingAlg::PowerIndexEnum(None) {
        let top_tier_nodes: Vec<NodeId> = fbas.all_nodes().iter().collect();
        let alg_with_tt = RankingAlg::PowerIndexEnum(Some(top_tier_nodes));
        rank_fbas(input.clone(), &fbas, alg_with_tt, qi_check, monitor)
    } else {
        rank_fbas(input.clone(), &fbas, alg, qi_check, monitor)
    };

    PerfDataPoint {
        top_tier_size: input.top_tier_size,
        run: input.run,
        duration,
        total_mem_in_gb: mem_info.0,
        used_mem_in_gb: mem_info.1,
        avail_mem_in_gb: mem_info.2,
        free_mem_in_gb: mem_info.3,
        total_swap_in_gb: mem_info.4,
        free_swap_in_gb: mem_info.5,
    }
}

fn write_csv(
    data_points: impl IntoIterator<Item = impl serde::Serialize>,
    output_path: &Option<PathBuf>,
    overwrite_allowed: bool,
) -> Result<(), Box<dyn Error>> {
    if let Some(path) = output_path {
        if !overwrite_allowed && path.exists() {
            Err(Box::new(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Output file exists, refusing to overwrite.",
            )))
        } else {
            write_csv_to_file(data_points, path)
        }
    } else {
        write_csv_to_stdout(data_points)
    }
}
