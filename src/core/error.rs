// GraphError — the single error type that crosses all layer boundaries.
//
// Each variant names the layer that produced it so callers can distinguish
// recoverable conditions (NodeNotFound) from hard failures (StorageIo).

use super::{edge::EdgeId, node::NodeId};

#[derive(Debug)]
pub enum GraphError {
    // ── Domain errors ─────────────────────────────────────────────────
    NodeNotFound(NodeId),
    EdgeNotFound(EdgeId),
    DuplicateNode(NodeId),
    DuplicateEdge(EdgeId),

    // ── Infrastructure errors ─────────────────────────────────────────
    StorageIo(String),
    SerializationError(String),
    DeserializationError(String),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::NodeNotFound(id) => write!(f, "node {id} not found"),
            GraphError::EdgeNotFound(id) => write!(f, "edge {id} not found"),
            GraphError::DuplicateNode(id) => write!(f, "node {id} already exists"),
            GraphError::DuplicateEdge(id) => write!(f, "edge {id} already exists"),
            GraphError::StorageIo(msg) => write!(f, "storage I/O error: {msg}"),
            GraphError::SerializationError(msg) => write!(f, "serialization error: {msg}"),
            GraphError::DeserializationError(msg) => write!(f, "deserialization error: {msg}"),
        }
    }
}

impl std::error::Error for GraphError {}

// ── Conversions from std I/O ──────────────────────────────────────────────────

impl From<std::io::Error> for GraphError {
    fn from(e: std::io::Error) -> Self {
        GraphError::StorageIo(e.to_string())
    }
}

impl From<serde_json::Error> for GraphError {
    fn from(e: serde_json::Error) -> Self {
        GraphError::SerializationError(e.to_string())
    }
}
