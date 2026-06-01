// NoCache — a pass-through cache that never stores anything.
//
// Useful for:
//   • Integration tests that must exercise the storage layer on every read
//   • Benchmarking the raw storage throughput without cache interference
//   • Demonstrating what performance looks like without the RAM layer
//
// Swap it in place of LruCache — the LayeredGraphDatabase is unchanged.

use crate::core::{
    edge::{Edge, EdgeId},
    node::{Node, NodeId},
};
use crate::ports::cache::CachePort;

pub struct NoCache;

impl CachePort for NoCache {
    fn get_node(&mut self, _id: NodeId) -> Option<Node> { None }
    fn put_node(&mut self, _node: Node) {}
    fn invalidate_node(&mut self, _id: NodeId) {}
    fn get_edge(&mut self, _id: EdgeId) -> Option<Edge> { None }
    fn put_edge(&mut self, _edge: Edge) {}
    fn invalidate_edge(&mut self, _id: EdgeId) {}
    fn clear(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::node::Node;
    use crate::ports::cache::CachePort;

    #[test]
    fn always_returns_none_for_nodes() {
        let mut cache = NoCache;
        cache.put_node(Node::new(NodeId(0), "Person"));
        assert!(cache.get_node(NodeId(0)).is_none());
    }

    #[test]
    fn always_returns_none_for_edges() {
        let mut cache = NoCache;
        assert!(cache.get_edge(EdgeId(0)).is_none());
    }

    #[test]
    fn invalidate_and_clear_do_not_panic() {
        let mut cache = NoCache;
        cache.invalidate_node(NodeId(0));
        cache.invalidate_edge(EdgeId(0));
        cache.clear();
    }
}
