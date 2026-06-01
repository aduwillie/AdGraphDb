// JsonFileStorage — newline-delimited JSON Write-Ahead Log (WAL).
//
// File format (one JSON object per line):
//
//   {"op":"UpsertNode","node":{...}}
//   {"op":"UpsertEdge","edge":{...}}
//   {"op":"DeleteNode","id":42}
//   {"op":"DeleteEdge","id":7}
//
// Reads replay all records from the top; later records shadow earlier ones.
// Deletes are tombstones — the entry is logically gone but bytes remain.
// `compact()` rewrites the file with only live state (no tombstones, no stale
// overwrites), keeping file size proportional to live data.
//
// Why JSON?  Human readable — open the file in any text editor to inspect the
// database state.  Ideal for debugging and learning.  The binary adapter gives
// the same guarantees with a smaller on-disk footprint.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
};
use crate::ports::storage::StoragePort;

// ── WAL record type ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum WalRecord {
    UpsertNode { node: Node },
    UpsertEdge { edge: Edge },
    DeleteNode { id: u64 },
    DeleteEdge { id: u64 },
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct JsonFileStorage {
    path: PathBuf,
    /// Append-only write handle kept open to avoid repeated open/close overhead.
    writer: File,
}

impl JsonFileStorage {
    /// Open (or create) the WAL file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GraphError> {
        let path = path.as_ref().to_path_buf();

        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        Ok(Self { path, writer })
    }

    fn append_record(&mut self, record: &WalRecord) -> Result<(), GraphError> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;
        Ok(())
    }

    /// Read and replay all WAL records, returning (live_nodes, live_edges).
    fn replay(&self) -> Result<(HashMap<NodeId, Node>, HashMap<EdgeId, Edge>), GraphError> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((HashMap::new(), HashMap::new()))
            }
            Err(e) => return Err(GraphError::StorageIo(e.to_string())),
        };

        let mut nodes: HashMap<NodeId, Node> = HashMap::new();
        let mut edges: HashMap<EdgeId, Edge> = HashMap::new();

        for (line_number, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|e| GraphError::StorageIo(e.to_string()))?;

            if line.trim().is_empty() {
                continue;
            }

            let record: WalRecord = serde_json::from_str(&line).map_err(|e| {
                GraphError::DeserializationError(format!("line {}: {e}", line_number + 1))
            })?;

            match record {
                WalRecord::UpsertNode { node } => { nodes.insert(node.id, node); }
                WalRecord::UpsertEdge { edge } => { edges.insert(edge.id, edge); }
                WalRecord::DeleteNode { id } => { nodes.remove(&NodeId(id)); }
                WalRecord::DeleteEdge { id } => { edges.remove(&EdgeId(id)); }
            }
        }

        Ok((nodes, edges))
    }
}

impl StoragePort for JsonFileStorage {
    fn save_node(&mut self, node: &Node) -> Result<(), GraphError> {
        self.append_record(&WalRecord::UpsertNode { node: node.clone() })
    }

    fn load_node(&self, id: NodeId) -> Result<Option<Node>, GraphError> {
        let (nodes, _) = self.replay()?;
        Ok(nodes.get(&id).cloned())
    }

    fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError> {
        self.append_record(&WalRecord::DeleteNode { id: id.0 })
    }

    fn save_edge(&mut self, edge: &Edge) -> Result<(), GraphError> {
        self.append_record(&WalRecord::UpsertEdge { edge: edge.clone() })
    }

    fn load_edge(&self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        let (_, edges) = self.replay()?;
        Ok(edges.get(&id).cloned())
    }

    fn delete_edge(&mut self, id: EdgeId) -> Result<(), GraphError> {
        self.append_record(&WalRecord::DeleteEdge { id: id.0 })
    }

    fn load_all_nodes(&self) -> Result<Vec<Node>, GraphError> {
        let (nodes, _) = self.replay()?;
        Ok(nodes.into_values().collect())
    }

    fn load_all_edges(&self) -> Result<Vec<Edge>, GraphError> {
        let (_, edges) = self.replay()?;
        Ok(edges.into_values().collect())
    }

    fn compact(&mut self) -> Result<(), GraphError> {
        let (nodes, edges) = self.replay()?;

        // Write the compacted state to a temporary file, then rename atomically.
        let tmp_path = self.path.with_extension("json.tmp");

        let mut tmp_file = File::create(&tmp_path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        for node in nodes.values() {
            let record = WalRecord::UpsertNode { node: node.clone() };
            let mut line = serde_json::to_string(&record)?;
            line.push('\n');
            tmp_file
                .write_all(line.as_bytes())
                .map_err(|e| GraphError::StorageIo(e.to_string()))?;
        }

        for edge in edges.values() {
            let record = WalRecord::UpsertEdge { edge: edge.clone() };
            let mut line = serde_json::to_string(&record)?;
            line.push('\n');
            tmp_file
                .write_all(line.as_bytes())
                .map_err(|e| GraphError::StorageIo(e.to_string()))?;
        }

        // Flush and close the temp file before renaming.
        drop(tmp_file);

        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        // Re-open the writer on the compacted file.
        self.writer = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{edge::EdgeId, node::NodeId};
    use crate::ports::storage::StoragePort;
    use crate::test_helpers::TempPath;

    fn sample_node(id: u64) -> Node {
        Node::new(NodeId(id), "Person")
            .with_property("name", format!("Node{id}"))
    }

    fn sample_edge(id: u64, src: u64, tgt: u64) -> Edge {
        Edge::new(EdgeId(id), NodeId(src), NodeId(tgt), "KNOWS", 1.0)
    }

    #[test]
    fn save_and_load_node_roundtrip() {
        let tmp = TempPath::new("json_node.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        let node = sample_node(0);
        storage.save_node(&node).unwrap();
        let loaded = storage.load_node(NodeId(0)).unwrap().unwrap();
        assert_eq!(loaded.id, NodeId(0));
        assert_eq!(loaded.label, "Person");
    }

    #[test]
    fn load_missing_node_returns_none() {
        let tmp = TempPath::new("json_missing.json");
        let storage = JsonFileStorage::open(tmp.path()).unwrap();
        assert!(storage.load_node(NodeId(99)).unwrap().is_none());
    }

    #[test]
    fn delete_node_makes_it_invisible() {
        let tmp = TempPath::new("json_delete.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        storage.save_node(&sample_node(0)).unwrap();
        storage.delete_node(NodeId(0)).unwrap();
        assert!(storage.load_node(NodeId(0)).unwrap().is_none());
    }

    #[test]
    fn upsert_node_overwrites_old_version() {
        let tmp = TempPath::new("json_upsert.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        storage.save_node(&sample_node(0)).unwrap();
        let updated = Node::new(NodeId(0), "City");
        storage.save_node(&updated).unwrap();
        let loaded = storage.load_node(NodeId(0)).unwrap().unwrap();
        assert_eq!(loaded.label, "City");
    }

    #[test]
    fn save_and_load_edge_roundtrip() {
        let tmp = TempPath::new("json_edge.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        storage.save_edge(&sample_edge(0, 1, 2)).unwrap();
        let loaded = storage.load_edge(EdgeId(0)).unwrap().unwrap();
        assert_eq!(loaded.source, NodeId(1));
        assert_eq!(loaded.target, NodeId(2));
    }

    #[test]
    fn delete_edge_makes_it_invisible() {
        let tmp = TempPath::new("json_del_edge.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        storage.save_edge(&sample_edge(0, 1, 2)).unwrap();
        storage.delete_edge(EdgeId(0)).unwrap();
        assert!(storage.load_edge(EdgeId(0)).unwrap().is_none());
    }

    #[test]
    fn load_all_nodes_excludes_deleted() {
        let tmp = TempPath::new("json_all.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        storage.save_node(&sample_node(0)).unwrap();
        storage.save_node(&sample_node(1)).unwrap();
        storage.delete_node(NodeId(0)).unwrap();
        let nodes = storage.load_all_nodes().unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, NodeId(1));
    }

    #[test]
    fn compact_reduces_tombstones() {
        let tmp = TempPath::new("json_compact.json");
        let mut storage = JsonFileStorage::open(tmp.path()).unwrap();
        for i in 0..5 { storage.save_node(&sample_node(i)).unwrap(); }
        storage.delete_node(NodeId(0)).unwrap();
        storage.delete_node(NodeId(1)).unwrap();
        storage.compact().unwrap();
        // After compaction: only nodes 2, 3, 4 should remain.
        let nodes = storage.load_all_nodes().unwrap();
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn data_survives_reopen() {
        let tmp = TempPath::new("json_reopen.json");
        {
            let mut s = JsonFileStorage::open(tmp.path()).unwrap();
            s.save_node(&sample_node(42)).unwrap();
        }
        // Reopen — simulates a restart.
        let s2 = JsonFileStorage::open(tmp.path()).unwrap();
        assert!(s2.load_node(NodeId(42)).unwrap().is_some());
    }
}
