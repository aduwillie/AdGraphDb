// BinaryFileStorage — hand-written binary WAL with a custom codec.
//
// This adapter is intentionally verbose so every byte is visible and
// understandable.  No external serialization library is used.
//
// ── File format ───────────────────────────────────────────────────────────────
//
//  Header (8 bytes):
//    [0..4]  magic  = b"AGDB"
//    [4..8]  version = 1u32 little-endian
//
//  Records (variable length, appended one after another):
//    [0]     record_type : u8
//              0x01 = UpsertNode
//              0x02 = UpsertEdge
//              0x03 = DeleteNode
//              0x04 = DeleteEdge
//    [1..]   payload (see encode_node / encode_edge)
//
// ── Node payload ──────────────────────────────────────────────────────────────
//    [u64]   node_id
//    [str]   label          (u32 length prefix + UTF-8 bytes)
//    [u32]   properties count
//    repeated:
//      [str]   key
//      [value] value
//
// ── Edge payload ──────────────────────────────────────────────────────────────
//    [u64]   edge_id
//    [u64]   source_id
//    [u64]   target_id
//    [str]   label
//    [f64]   weight
//    [u32]   properties count
//    repeated: [str] key + [value] value
//
// ── Value encoding ────────────────────────────────────────────────────────────
//    [u8]  tag
//      0x00 = Null    (no payload)
//      0x01 = Boolean [u8: 0=false / 1=true]
//      0x02 = Integer [i64 little-endian]
//      0x03 = Float   [f64 little-endian]
//      0x04 = Text    [str]
//
// ── DeleteNode / DeleteEdge payload ──────────────────────────────────────────
//    [u64]   id

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
    value::Value,
};
use crate::ports::storage::StoragePort;

const MAGIC: &[u8; 4] = b"AGDB";
const VERSION: u32 = 1;

const RECORD_UPSERT_NODE: u8 = 0x01;
const RECORD_UPSERT_EDGE: u8 = 0x02;
const RECORD_DELETE_NODE: u8 = 0x03;
const RECORD_DELETE_EDGE: u8 = 0x04;

const VALUE_NULL: u8 = 0x00;
const VALUE_BOOLEAN: u8 = 0x01;
const VALUE_INTEGER: u8 = 0x02;
const VALUE_FLOAT: u8 = 0x03;
const VALUE_TEXT: u8 = 0x04;

// ── Low-level write helpers ───────────────────────────────────────────────────

fn write_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_i64(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f64(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Encode a string as: [u32 byte-length][UTF-8 bytes]
fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

fn write_value(buf: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => write_u8(buf, VALUE_NULL),
        Value::Boolean(b) => {
            write_u8(buf, VALUE_BOOLEAN);
            write_u8(buf, if *b { 1 } else { 0 });
        }
        Value::Integer(i) => {
            write_u8(buf, VALUE_INTEGER);
            write_i64(buf, *i);
        }
        Value::Float(f) => {
            write_u8(buf, VALUE_FLOAT);
            write_f64(buf, *f);
        }
        Value::Text(s) => {
            write_u8(buf, VALUE_TEXT);
            write_str(buf, s);
        }
    }
}

fn write_properties(buf: &mut Vec<u8>, props: &HashMap<String, Value>) {
    write_u32(buf, props.len() as u32);
    for (key, value) in props {
        write_str(buf, key);
        write_value(buf, value);
    }
}

// ── Low-level read helpers ────────────────────────────────────────────────────

fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, GraphError> {
    if *pos >= data.len() {
        return Err(eof("u8"));
    }
    let v = data[*pos];
    *pos += 1;
    Ok(v)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, GraphError> {
    let bytes = read_bytes(data, pos, 4)?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, GraphError> {
    let bytes = read_bytes(data, pos, 8)?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_i64(data: &[u8], pos: &mut usize) -> Result<i64, GraphError> {
    let bytes = read_bytes(data, pos, 8)?;
    Ok(i64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_f64(data: &[u8], pos: &mut usize) -> Result<f64, GraphError> {
    let bytes = read_bytes(data, pos, 8)?;
    Ok(f64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], GraphError> {
    if *pos + n > data.len() {
        return Err(eof(&format!("{n} bytes")));
    }
    let slice = &data[*pos..*pos + n];
    *pos += n;
    Ok(slice)
}

fn read_str(data: &[u8], pos: &mut usize) -> Result<String, GraphError> {
    let len = read_u32(data, pos)? as usize;
    let bytes = read_bytes(data, pos, len)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|e| GraphError::DeserializationError(format!("invalid UTF-8: {e}")))
}

fn read_value(data: &[u8], pos: &mut usize) -> Result<Value, GraphError> {
    let tag = read_u8(data, pos)?;
    match tag {
        VALUE_NULL => Ok(Value::Null),
        VALUE_BOOLEAN => Ok(Value::Boolean(read_u8(data, pos)? != 0)),
        VALUE_INTEGER => Ok(Value::Integer(read_i64(data, pos)?)),
        VALUE_FLOAT => Ok(Value::Float(read_f64(data, pos)?)),
        VALUE_TEXT => Ok(Value::Text(read_str(data, pos)?)),
        other => Err(GraphError::DeserializationError(format!(
            "unknown value tag: 0x{other:02x}"
        ))),
    }
}

fn read_properties(data: &[u8], pos: &mut usize) -> Result<HashMap<String, Value>, GraphError> {
    let count = read_u32(data, pos)? as usize;
    let mut map = HashMap::with_capacity(count);
    for _ in 0..count {
        let key = read_str(data, pos)?;
        let value = read_value(data, pos)?;
        map.insert(key, value);
    }
    Ok(map)
}

fn eof(what: &str) -> GraphError {
    GraphError::DeserializationError(format!("unexpected end of file reading {what}"))
}

// ── Node / Edge codec ─────────────────────────────────────────────────────────

fn encode_node(node: &Node) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_UPSERT_NODE);
    write_u64(&mut buf, node.id.0);
    write_str(&mut buf, &node.label);
    write_properties(&mut buf, &node.properties);
    buf
}

fn encode_edge(edge: &Edge) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_UPSERT_EDGE);
    write_u64(&mut buf, edge.id.0);
    write_u64(&mut buf, edge.source.0);
    write_u64(&mut buf, edge.target.0);
    write_str(&mut buf, &edge.label);
    write_f64(&mut buf, edge.weight);
    write_properties(&mut buf, &edge.properties);
    buf
}

fn encode_delete_node(id: NodeId) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_DELETE_NODE);
    write_u64(&mut buf, id.0);
    buf
}

fn encode_delete_edge(id: EdgeId) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_DELETE_EDGE);
    write_u64(&mut buf, id.0);
    buf
}

fn decode_node(data: &[u8], pos: &mut usize) -> Result<Node, GraphError> {
    let id = NodeId(read_u64(data, pos)?);
    let label = read_str(data, pos)?;
    let properties = read_properties(data, pos)?;
    Ok(Node { id, label, properties })
}

fn decode_edge(data: &[u8], pos: &mut usize) -> Result<Edge, GraphError> {
    let id = EdgeId(read_u64(data, pos)?);
    let source = NodeId(read_u64(data, pos)?);
    let target = NodeId(read_u64(data, pos)?);
    let label = read_str(data, pos)?;
    let weight = read_f64(data, pos)?;
    let properties = read_properties(data, pos)?;
    Ok(Edge { id, source, target, label, weight, properties })
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct BinaryFileStorage {
    path: PathBuf,
    writer: File,
}

impl BinaryFileStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GraphError> {
        let path = path.as_ref().to_path_buf();
        let file_exists = path.exists();

        let mut writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        if !file_exists {
            // Write the file header on first creation.
            let mut header = Vec::with_capacity(8);
            header.extend_from_slice(MAGIC);
            write_u32(&mut header, VERSION);
            writer
                .write_all(&header)
                .map_err(|e| GraphError::StorageIo(e.to_string()))?;
        }

        Ok(Self { path, writer })
    }

    fn append_bytes(&mut self, bytes: &[u8]) -> Result<(), GraphError> {
        self.writer
            .write_all(bytes)
            .map_err(|e| GraphError::StorageIo(e.to_string()))
    }

    fn replay(&self) -> Result<(HashMap<NodeId, Node>, HashMap<EdgeId, Edge>), GraphError> {
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
            Err(e) => return Err(GraphError::StorageIo(e.to_string())),
        };

        if data.len() < 8 {
            return Ok(Default::default());
        }

        // Validate header.
        if &data[0..4] != MAGIC {
            return Err(GraphError::DeserializationError(
                "invalid magic bytes — not an AGDB binary file".into(),
            ));
        }
        let file_version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if file_version != VERSION {
            return Err(GraphError::DeserializationError(format!(
                "unsupported file version {file_version}"
            )));
        }

        let mut pos = 8; // Skip header.
        let mut nodes: HashMap<NodeId, Node> = HashMap::new();
        let mut edges: HashMap<EdgeId, Edge> = HashMap::new();

        while pos < data.len() {
            let record_type = read_u8(&data, &mut pos)?;
            match record_type {
                RECORD_UPSERT_NODE => {
                    let node = decode_node(&data, &mut pos)?;
                    nodes.insert(node.id, node);
                }
                RECORD_UPSERT_EDGE => {
                    let edge = decode_edge(&data, &mut pos)?;
                    edges.insert(edge.id, edge);
                }
                RECORD_DELETE_NODE => {
                    let id = NodeId(read_u64(&data, &mut pos)?);
                    nodes.remove(&id);
                }
                RECORD_DELETE_EDGE => {
                    let id = EdgeId(read_u64(&data, &mut pos)?);
                    edges.remove(&id);
                }
                other => {
                    return Err(GraphError::DeserializationError(format!(
                        "unknown record type 0x{other:02x} at byte offset {pos}"
                    )));
                }
            }
        }

        Ok((nodes, edges))
    }
}

impl StoragePort for BinaryFileStorage {
    fn save_node(&mut self, node: &Node) -> Result<(), GraphError> {
        let bytes = encode_node(node);
        self.append_bytes(&bytes)
    }

    fn load_node(&self, id: NodeId) -> Result<Option<Node>, GraphError> {
        let (nodes, _) = self.replay()?;
        Ok(nodes.get(&id).cloned())
    }

    fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError> {
        let bytes = encode_delete_node(id);
        self.append_bytes(&bytes)
    }

    fn save_edge(&mut self, edge: &Edge) -> Result<(), GraphError> {
        let bytes = encode_edge(edge);
        self.append_bytes(&bytes)
    }

    fn load_edge(&self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        let (_, edges) = self.replay()?;
        Ok(edges.get(&id).cloned())
    }

    fn delete_edge(&mut self, id: EdgeId) -> Result<(), GraphError> {
        let bytes = encode_delete_edge(id);
        self.append_bytes(&bytes)
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

        let tmp_path = self.path.with_extension("bin.tmp");
        let mut tmp = File::create(&tmp_path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        // Write header.
        let mut header = Vec::with_capacity(8);
        header.extend_from_slice(MAGIC);
        write_u32(&mut header, VERSION);
        tmp.write_all(&header)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

        for node in nodes.values() {
            tmp.write_all(&encode_node(node))
                .map_err(|e| GraphError::StorageIo(e.to_string()))?;
        }
        for edge in edges.values() {
            tmp.write_all(&encode_edge(edge))
                .map_err(|e| GraphError::StorageIo(e.to_string()))?;
        }
        drop(tmp);

        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| GraphError::StorageIo(e.to_string()))?;

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
    use crate::core::{node::NodeId, value::Value};
    use crate::ports::storage::StoragePort;
    use crate::test_helpers::TempPath;

    fn sample_node(id: u64) -> Node {
        Node::new(NodeId(id), "Place")
            .with_property("name", format!("place{id}"))
            .with_property("score", id as i64)
    }

    fn sample_edge(id: u64, src: u64, tgt: u64) -> Edge {
        Edge::new(EdgeId(id), NodeId(src), NodeId(tgt), "CONNECTS", id as f64)
    }

    #[test]
    fn save_and_load_node_roundtrip() {
        let tmp = TempPath::new("bin_node.bin");
        let mut s = BinaryFileStorage::open(tmp.path()).unwrap();
        s.save_node(&sample_node(7)).unwrap();
        let loaded = s.load_node(NodeId(7)).unwrap().unwrap();
        assert_eq!(loaded.id, NodeId(7));
        assert_eq!(loaded.label, "Place");
        assert_eq!(loaded.properties["score"], Value::Integer(7));
    }

    #[test]
    fn load_missing_returns_none() {
        let tmp = TempPath::new("bin_miss.bin");
        let s = BinaryFileStorage::open(tmp.path()).unwrap();
        assert!(s.load_node(NodeId(0)).unwrap().is_none());
    }

    #[test]
    fn delete_node_tombstones_entry() {
        let tmp = TempPath::new("bin_del.bin");
        let mut s = BinaryFileStorage::open(tmp.path()).unwrap();
        s.save_node(&sample_node(1)).unwrap();
        s.delete_node(NodeId(1)).unwrap();
        assert!(s.load_node(NodeId(1)).unwrap().is_none());
    }

    #[test]
    fn upsert_overwrites() {
        let tmp = TempPath::new("bin_upsert.bin");
        let mut s = BinaryFileStorage::open(tmp.path()).unwrap();
        s.save_node(&sample_node(0)).unwrap();
        let overwrite = Node::new(NodeId(0), "Updated");
        s.save_node(&overwrite).unwrap();
        let loaded = s.load_node(NodeId(0)).unwrap().unwrap();
        assert_eq!(loaded.label, "Updated");
    }

    #[test]
    fn edge_roundtrip_preserves_weight() {
        let tmp = TempPath::new("bin_edg.bin");
        let mut s = BinaryFileStorage::open(tmp.path()).unwrap();
        s.save_edge(&sample_edge(0, 10, 20)).unwrap();
        let loaded = s.load_edge(EdgeId(0)).unwrap().unwrap();
        assert!((loaded.weight - 0.0_f64).abs() < f64::EPSILON);
        assert_eq!(loaded.source, NodeId(10));
    }

    #[test]
    fn compact_leaves_only_live_data() {
        let tmp = TempPath::new("bin_compact.bin");
        let mut s = BinaryFileStorage::open(tmp.path()).unwrap();
        for i in 0..4 { s.save_node(&sample_node(i)).unwrap(); }
        s.delete_node(NodeId(0)).unwrap();
        s.delete_node(NodeId(3)).unwrap();
        s.compact().unwrap();
        let nodes = s.load_all_nodes().unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn all_value_types_survive_roundtrip() {
        let tmp = TempPath::new("bin_vals.bin");
        let mut s = BinaryFileStorage::open(tmp.path()).unwrap();
        let node = Node::new(NodeId(0), "AllTypes")
            .with_property("null",  Value::Null)
            .with_property("bool",  Value::Boolean(true))
            .with_property("int",   Value::Integer(-42))
            .with_property("float", Value::Float(3.14))
            .with_property("text",  Value::Text("hello".into()));
        s.save_node(&node).unwrap();
        let loaded = s.load_node(NodeId(0)).unwrap().unwrap();
        assert_eq!(loaded.properties["null"],  Value::Null);
        assert_eq!(loaded.properties["bool"],  Value::Boolean(true));
        assert_eq!(loaded.properties["int"],   Value::Integer(-42));
        assert_eq!(loaded.properties["text"],  Value::Text("hello".into()));
    }

    #[test]
    fn data_survives_reopen() {
        let tmp = TempPath::new("bin_reopen.bin");
        { BinaryFileStorage::open(tmp.path()).unwrap().save_node(&sample_node(55)).unwrap(); }
        let s2 = BinaryFileStorage::open(tmp.path()).unwrap();
        assert!(s2.load_node(NodeId(55)).unwrap().is_some());
    }
}
