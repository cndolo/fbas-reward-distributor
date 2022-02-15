use crate::*;

use fbas_analyzer::NodeId;
use std::collections::HashMap;

/// Distribute rewards according to NodeRank scores and return a map of NodeId, score, reward
/// TODO: Compute NodeRank in the function instead of passing the scores
pub fn compute_reward_distribution(scores: &[Score], reward: f64) -> HashMap<NodeId, (Score, f64)> {
    let mut rewards = HashMap::default();
    let node_rank_sum: Score = scores.iter().map(|&v| v as f64).sum();
    for (node, node_score) in scores.iter().enumerate() {
        // normalise values nr/sum(nr)
        let reward_factor = node_score / node_rank_sum;
        let reward = reward_factor * reward;
        rewards.insert(node, (scores[node], reward));
    }
    rewards
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbas_analyzer::Fbas;
    use std::path::Path;

    #[test]
    fn allocate_rewards_simple_fbas() {
        let fbas = Fbas::from_json_file(Path::new("test_data/correct_trivial.json"));
        let all_nodes: Vec<NodeId> = (0..fbas.all_nodes().len()).collect();
        let reward = 1.0;
        let noderanks = compute_node_rank_for_fbas(&all_nodes, &fbas);
        let actual = compute_reward_distribution(&noderanks, reward);
        let expected = HashMap::from([
            (0, (noderanks[0], reward / 3.0)),
            (1, (noderanks[1], reward / 3.0)),
            (2, (noderanks[1], reward / 3.0)),
        ]);
        assert_eq!(actual, expected);
    }
}
