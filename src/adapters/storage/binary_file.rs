// BinaryFileStorage — hand-written binary WAL with a custom codec.
//
// This adapter is intentionally verbose so every byte is visible and
// understandable.  No external serialization library is used.
//
// ── File format v2 ───────────────────────────────────────────────────────────
//
//  Header (8 bytes):
//    [0..4]  magic  = b"AGDB"
//    [4..8]  version = 2u32 little-endian
//
//  Records (variable length, appended one after another):
//    [0]     record_type : u8
//              0x01 = UpsertNode
//              0x02 = UpsertEdge
//              0x03 = DeleteNode
//              0x04 = DeleteEdge
//              0x05 = BeginTxn    [u64: txn_id]
//              0x06 = CommitTxn   [u64: txn_id]
//              0x07 = RollbackTxn [u64: txn_id]
//    [1..]   payload
//    [-4..]  Adler-32 checksum of (record_type byte + payload), u32 LE
//
// Adler-32 is computed over every byte from record_type through end of payload.
// On replay, a checksum mismatch causes the record to be skipped with a warning.
//
// WAL transaction semantics:
//   Records between BeginTxn and CommitTxn are buffered during replay.
//   If EOF is reached without a matching CommitTxn, the buffered records
//   are discarded (rolled back).  This makes multi-op commits crash-safe.
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
const VERSION: u32 = 2;

const RECORD_UPSERT_NODE:  u8 = 0x01;
const RECORD_UPSERT_EDGE:  u8 = 0x02;
const RECORD_DELETE_NODE:  u8 = 0x03;
const RECORD_DELETE_EDGE:  u8 = 0x04;
const RECORD_BEGIN_TXN:    u8 = 0x05;
const RECORD_COMMIT_TXN:   u8 = 0x06;
const RECORD_ROLLBACK_TXN: u8 = 0x07;

// ── Adler-32 checksum ─────────────────────────────────────────────────────────
//
// A simple and fast checksum algorithm (RFC 1950).
// Detects single-bit errors and most burst errors.
// Appended to every record as a 4-byte LE u32.

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a.wrapping_add(byte as u32)) % MOD;
        b = (b.wrapping_add(a))           % MOD;
    }
    (b << 16) | a
}

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
//
// Every record ends with a 4-byte Adler-32 checksum of all bytes in that
// record (including the record_type byte).  The checksum is computed AFTER
// the payload is assembled so it covers everything.

fn with_checksum(mut buf: Vec<u8>) -> Vec<u8> {
    let csum = adler32(&buf);
    write_u32(&mut buf, csum);
    buf
}

fn encode_node(node: &Node) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_UPSERT_NODE);
    write_u64(&mut buf, node.id.0);
    write_str(&mut buf, &node.label);
    write_properties(&mut buf, &node.properties);
    with_checksum(buf)
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
    with_checksum(buf)
}

fn encode_delete_node(id: NodeId) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_DELETE_NODE);
    write_u64(&mut buf, id.0);
    with_checksum(buf)
}

fn encode_delete_edge(id: EdgeId) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, RECORD_DELETE_EDGE);
    write_u64(&mut buf, id.0);
    with_checksum(buf)
}

fn encode_txn_marker(record_type: u8, txn_id: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    write_u8(&mut buf, record_type);
    write_u64(&mut buf, txn_id);
    with_checksum(buf)
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

        let use_checksums = file_version >= 2;

        let mut pos = 8; // Skip header.
        let mut nodes: HashMap<NodeId, Node> = HashMap::new();
        let mut edges: HashMap<EdgeId, Edge> = HashMap::new();

        // In-flight transaction buffer: BeginTxn → CommitTxn bracket.
        // If CommitTxn is never seen (crash), these are discarded on EOF.
        let mut txn_buffer: Option<(u64, Vec<(NodeId, Node)>, Vec<(EdgeId, Edge)>,
                                         Vec<NodeId>, Vec<EdgeId>)> = None;

        while pos < data.len() {
            let record_start = pos;
            let record_type = match read_u8(&data, &mut pos) {
                Ok(t) => t,
                Err(_) => break, // Truncated file — stop gracefully
            };

            // ── Decode payload ────────────────────────────────────────────────
            let decoded = decode_record(record_type, &data, &mut pos);

            // ── Verify checksum if v2 ─────────────────────────────────────────
            if use_checksums {
                match read_u32(&data, &mut pos) {
                    Ok(stored_csum) => {
                        let payload_bytes = &data[record_start..pos - 4];
                        let computed = adler32(payload_bytes);
                        if computed != stored_csum {
                            eprintln!(
                                "[BinaryFileStorage] checksum mismatch at offset {record_start} \
                                 (stored={stored_csum:#010x}, computed={computed:#010x}) — skipping record"
                            );
                            continue;
                        }
                    }
                    Err(_) => break, // Truncated checksum — stop
                }
            }

            // ── Apply or buffer ────────────────────────────────────────────────
            match decoded {
                Ok(RecordData::BeginTxn(txn_id)) => {
                    // Start buffering.
                    txn_buffer = Some((txn_id, vec![], vec![], vec![], vec![]));
                }
                Ok(RecordData::CommitTxn(txn_id)) => {
                    if let Some((bid, n_ins, e_ins, n_del, e_del)) = txn_buffer.take() {
                        if bid == txn_id {
                            // Apply buffered records.
                            for (id, node) in n_ins { nodes.insert(id, node); }
                            for (id, edge) in e_ins { edges.insert(id, edge); }
                            for id in n_del { nodes.remove(&id); }
                            for id in e_del { edges.remove(&id); }
                        }
                        // else: mismatched txn_id — discard
                    }
                }
                Ok(RecordData::RollbackTxn(_)) => {
                    txn_buffer = None; // Discard buffered records
                }
                Ok(record) => {
                    apply_or_buffer(record, &mut nodes, &mut edges, &mut txn_buffer);
                }
                Err(e) => {
                    eprintln!("[BinaryFileStorage] decode error at offset {record_start}: {e} — stopping replay");
                    break;
                }
            }
        }

        // If txn_buffer is still Some at EOF, the BeginTxn had no CommitTxn
        // (process crashed mid-commit).  Discard the buffered records.
        if txn_buffer.is_some() {
            eprintln!("[BinaryFileStorage] incomplete transaction at EOF — rolled back");
        }

        Ok((nodes, edges))
    }

    pub fn wal_file_size(&self) -> u64 {
        self.path.metadata().map(|m| m.len()).unwrap_or(0)
    }
}

// ── Decoded record variants ────────────────────────────────────────────────────

enum RecordData {
    UpsertNode(Node),
    UpsertEdge(Edge),
    DeleteNode(NodeId),
    DeleteEdge(EdgeId),
    BeginTxn(u64),
    CommitTxn(u64),
    RollbackTxn(#[allow(dead_code)] u64),
}

fn decode_record(record_type: u8, data: &[u8], pos: &mut usize) -> Result<RecordData, GraphError> {
    match record_type {
        RECORD_UPSERT_NODE  => Ok(RecordData::UpsertNode(decode_node(data, pos)?)),
        RECORD_UPSERT_EDGE  => Ok(RecordData::UpsertEdge(decode_edge(data, pos)?)),
        RECORD_DELETE_NODE  => Ok(RecordData::DeleteNode(NodeId(read_u64(data, pos)?))),
        RECORD_DELETE_EDGE  => Ok(RecordData::DeleteEdge(EdgeId(read_u64(data, pos)?))),
        RECORD_BEGIN_TXN    => Ok(RecordData::BeginTxn(read_u64(data, pos)?)),
        RECORD_COMMIT_TXN   => Ok(RecordData::CommitTxn(read_u64(data, pos)?)),
        RECORD_ROLLBACK_TXN => Ok(RecordData::RollbackTxn(read_u64(data, pos)?)),
        other => Err(GraphError::DeserializationError(format!(
            "unknown record type 0x{other:02x}"
        ))),
    }
}

type TxnBuffer = Option<(u64, Vec<(NodeId, Node)>, Vec<(EdgeId, Edge)>, Vec<NodeId>, Vec<EdgeId>)>;

fn apply_or_buffer(
    record: RecordData,
    nodes:  &mut HashMap<NodeId, Node>,
    edges:  &mut HashMap<EdgeId, Edge>,
    buf:    &mut TxnBuffer,
) {
    match buf {
        None => {
            // No active transaction — apply immediately.
            match record {
                RecordData::UpsertNode(n) => { nodes.insert(n.id, n); }
                RecordData::UpsertEdge(e) => { edges.insert(e.id, e); }
                RecordData::DeleteNode(id) => { nodes.remove(&id); }
                RecordData::DeleteEdge(id) => { edges.remove(&id); }
                _ => {}
            }
        }
        Some((_, n_ins, e_ins, n_del, e_del)) => {
            // Inside a transaction — buffer for later commit.
            match record {
                RecordData::UpsertNode(n) => n_ins.push((n.id, n)),
                RecordData::UpsertEdge(e) => e_ins.push((e.id, e)),
                RecordData::DeleteNode(id) => n_del.push(id),
                RecordData::DeleteEdge(id) => e_del.push(id),
                _ => {}
            }
        }
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

    fn wal_size_bytes(&self) -> u64 {
        self.wal_file_size()
    }

    fn begin_wal_transaction(&mut self, txn_id: u64) -> Result<(), GraphError> {
        self.append_bytes(&encode_txn_marker(RECORD_BEGIN_TXN, txn_id))
    }

    fn commit_wal_transaction(&mut self, txn_id: u64) -> Result<(), GraphError> {
        self.append_bytes(&encode_txn_marker(RECORD_COMMIT_TXN, txn_id))
    }

    fn rollback_wal_transaction(&mut self, txn_id: u64) -> Result<(), GraphError> {
        self.append_bytes(&encode_txn_marker(RECORD_ROLLBACK_TXN, txn_id))
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
