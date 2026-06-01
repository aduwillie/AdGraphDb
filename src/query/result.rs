// QueryResult — the value returned from executing any QueryCommand.
//
// The Display impl lets callers print results in a human-readable table without
// knowing which variant they received.

use crate::core::{edge::Edge, node::{Node, NodeId}};

#[derive(Debug)]
pub enum QueryResult {
    Nodes(Vec<Node>),
    Edges(Vec<Edge>),
    /// Ordered list of node IDs produced by a traversal.
    Traversal(Vec<NodeId>),
    /// The node sequence of a shortest path and its total weight.
    Path { nodes: Vec<NodeId>, total_weight: f64 },
    /// A single node (from GET NODE).
    SingleNode(Option<Node>),
    /// A single edge (from GET EDGE).
    SingleEdge(Option<Edge>),
    Count(usize),
    Empty,
}

impl std::fmt::Display for QueryResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryResult::Nodes(nodes) => {
                writeln!(f, "Nodes ({}):", nodes.len())?;
                for node in nodes {
                    writeln!(f, "  {node}")?;
                }
                Ok(())
            }
            QueryResult::Edges(edges) => {
                writeln!(f, "Edges ({}):", edges.len())?;
                for edge in edges {
                    writeln!(f, "  {edge}")?;
                }
                Ok(())
            }
            QueryResult::Traversal(ids) => {
                let joined: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                write!(f, "Traversal [{}]: {}", ids.len(), joined.join(" → "))
            }
            QueryResult::Path { nodes, total_weight } => {
                let joined: Vec<String> = nodes.iter().map(|id| id.to_string()).collect();
                write!(f, "Path ({total_weight:.2}): {}", joined.join(" → "))
            }
            QueryResult::SingleNode(Some(node)) => write!(f, "{node}"),
            QueryResult::SingleNode(None) => write!(f, "(not found)"),
            QueryResult::SingleEdge(Some(edge)) => write!(f, "{edge}"),
            QueryResult::SingleEdge(None) => write!(f, "(not found)"),
            QueryResult::Count(n) => write!(f, "Count: {n}"),
            QueryResult::Empty => write!(f, "(empty)"),
        }
    }
}
