// IdGenerator — monotonically increasing ID source.
//
// The database holds one instance and seeds it from the highest ID seen when
// loading from storage, ensuring no collisions after a restart.

use super::{edge::EdgeId, node::NodeId};

// Clone is needed so Transaction can take a snapshot of the counter state
// and allocate IDs independently before commit.
#[derive(Debug, Default, Clone)]
pub struct IdGenerator {
    next_node: u64,
    next_edge: u64,
}

impl IdGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_node_id(&mut self) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node += 1;
        id
    }

    pub fn next_edge_id(&mut self) -> EdgeId {
        let id = EdgeId(self.next_edge);
        self.next_edge += 1;
        id
    }

    /// Call after loading persisted data so new IDs never collide with old ones.
    pub fn seed_from_node(&mut self, seen: NodeId) {
        if seen.0 >= self.next_node {
            self.next_node = seen.0 + 1;
        }
    }

    pub fn seed_from_edge(&mut self, seen: EdgeId) {
        if seen.0 >= self.next_edge {
            self.next_edge = seen.0 + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_sequential_node_ids() {
        let mut gen = IdGenerator::new();
        assert_eq!(gen.next_node_id(), NodeId(0));
        assert_eq!(gen.next_node_id(), NodeId(1));
        assert_eq!(gen.next_node_id(), NodeId(2));
    }

    #[test]
    fn generates_sequential_edge_ids() {
        let mut gen = IdGenerator::new();
        assert_eq!(gen.next_edge_id(), EdgeId(0));
        assert_eq!(gen.next_edge_id(), EdgeId(1));
    }

    #[test]
    fn node_and_edge_counters_are_independent() {
        let mut gen = IdGenerator::new();
        gen.next_node_id();
        gen.next_node_id();
        assert_eq!(gen.next_edge_id(), EdgeId(0)); // edge counter unaffected
    }

    #[test]
    fn seed_from_node_advances_counter() {
        let mut gen = IdGenerator::new();
        gen.seed_from_node(NodeId(9));
        assert_eq!(gen.next_node_id(), NodeId(10)); // next must be 9+1
    }

    #[test]
    fn seed_from_node_does_not_regress() {
        let mut gen = IdGenerator::new();
        gen.next_node_id(); // now at 1
        gen.seed_from_node(NodeId(0)); // 0 < 1, should be ignored
        assert_eq!(gen.next_node_id(), NodeId(1));
    }

    #[test]
    fn seed_from_edge_advances_counter() {
        let mut gen = IdGenerator::new();
        gen.seed_from_edge(EdgeId(4));
        assert_eq!(gen.next_edge_id(), EdgeId(5));
    }

    #[test]
    fn multiple_seeds_take_highest() {
        let mut gen = IdGenerator::new();
        gen.seed_from_node(NodeId(3));
        gen.seed_from_node(NodeId(7));
        gen.seed_from_node(NodeId(5));
        assert_eq!(gen.next_node_id(), NodeId(8));
    }
}
