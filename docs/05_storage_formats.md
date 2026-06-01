# 05 — Storage Formats

AdGraphDb ships two WAL adapters with different on-disk encodings.
Both implement `StoragePort` identically — only the bytes differ.

---

## JSON file format (`JsonFileStorage`)

### Structure

One JSON object per line (**NDJSON** / **JSON Lines** format).
Each line is one WAL record.

```json
{"op":"UpsertNode","node":{"id":{"0":0},"label":"City","properties":{"name":{"Text":"London"}}}}
{"op":"UpsertEdge","edge":{"id":{"0":0},"source":{"0":0},"target":{"0":1},"label":"RAIL","weight":457.0,"properties":{}}}
{"op":"DeleteNode","id":0}
{"op":"DeleteEdge","id":2}
```

### Record schema

```
UpsertNode  { "op": "UpsertNode", "node": <Node> }
UpsertEdge  { "op": "UpsertEdge", "edge": <Edge> }
DeleteNode  { "op": "DeleteNode", "id": <u64> }
DeleteEdge  { "op": "DeleteEdge", "id": <u64> }
```

The `"op"` field drives `serde`'s `#[serde(tag = "op")]` enum dispatch.

### Reading and debugging the JSON WAL

```bash
# Inspect current state
cat graph.json | python -m json.tool

# Count live nodes (subtract deleted ones manually)
grep '"op":"UpsertNode"' graph.json | wc -l

# Pretty-print one record
head -1 graph.json | python -m json.tool
```

### Advantages of JSON

- Human readable — open in any text editor
- Self-describing — field names are in the file
- Easy to write migration scripts in Python/jq
- Excellent for debugging a new storage adapter

### Disadvantages

- Larger files (field names repeated for every record)
- Slower to parse than binary
- Floating-point precision can vary across JSON parsers

---

## Binary file format (`BinaryFileStorage`)

### Overview

The binary format uses a hand-written codec (no external library).
All integers are **little-endian**. No padding or alignment bytes.

### File header (8 bytes)

```
Offset  Size  Value       Description
──────  ────  ─────────   ─────────────────────────────
0       4     b"AGDB"     Magic bytes — identifies file type
4       4     0x01000000  Version = 1 (u32 little-endian)
```

The magic bytes prevent accidentally reading a different binary file as a
valid AdGraphDb WAL.

### Record layout

Each record begins with a 1-byte type tag:

```
0x01 → UpsertNode   payload follows
0x02 → UpsertEdge   payload follows
0x03 → DeleteNode   [u64 node_id]
0x04 → DeleteEdge   [u64 edge_id]
```

### Node payload (after the 0x01 tag)

```
┌───────────────────────────────────────────┐
│ node_id          8 bytes  u64 LE          │
│ label_len        4 bytes  u32 LE          │
│ label            N bytes  UTF-8           │
│ property_count   4 bytes  u32 LE          │
│   ┌─────────────────────────────────────┐ │
│   │ key_len    4 bytes  u32 LE          │ │
│   │ key        N bytes  UTF-8           │ │
│   │ value_tag  1 byte   (see below)     │ │
│   │ value_data variable                 │ │
│   └─────────────────────────────────────┘ │
│   ... repeated property_count times       │
└───────────────────────────────────────────┘
```

### Edge payload (after the 0x02 tag)

```
┌─────────────────────────────────────────────┐
│ edge_id          8 bytes  u64 LE            │
│ source_id        8 bytes  u64 LE            │
│ target_id        8 bytes  u64 LE            │
│ label_len        4 bytes  u32 LE            │
│ label            N bytes  UTF-8             │
│ weight           8 bytes  f64 LE (IEEE 754) │
│ property_count   4 bytes  u32 LE            │
│   ... same layout as node properties        │
└─────────────────────────────────────────────┘
```

### Value encoding (tag + optional payload)

| Tag byte | Type | Payload |
|----------|------|---------|
| `0x00` | Null | (none — 0 bytes) |
| `0x01` | Boolean | 1 byte: `0x00` = false, `0x01` = true |
| `0x02` | Integer | 8 bytes: i64 little-endian |
| `0x03` | Float | 8 bytes: f64 little-endian (IEEE 754) |
| `0x04` | Text | 4 bytes (u32 length) + N bytes UTF-8 |

### Primitive encoding summary

| Rust type | Wire size | Format |
|-----------|-----------|--------|
| `u8` | 1 byte | raw |
| `u32` | 4 bytes | little-endian |
| `u64` | 8 bytes | little-endian |
| `i64` | 8 bytes | little-endian, two's complement |
| `f64` | 8 bytes | little-endian IEEE 754 double |
| `String` | 4 + N bytes | u32 length prefix + UTF-8 bytes |

### Annotated example

Node: `{ id: N0, label: "City", properties: { "name": Value::Text("London") } }`

```
01             ← record type: UpsertNode (0x01)
00 00 00 00
00 00 00 00    ← node_id = 0 (u64 LE)
04 00 00 00    ← label byte length = 4
43 69 74 79    ← "City" (UTF-8)
01 00 00 00    ← property_count = 1
04 00 00 00    ← key byte length = 4
6E 61 6D 65    ← "name" (UTF-8)
04             ← value tag: Text (0x04)
06 00 00 00    ← text byte length = 6
4C 6F 6E 64
6F 6E          ← "London" (UTF-8)
```

Total: **44 bytes** for this node record.

Equivalent JSON record: ~90 bytes (2× larger).

### Reading the binary WAL

```bash
# Verify magic bytes
xxd graph.bin | head -1
# 00000000: 4147 4442 0100 0000 ...   ← AGDB + version

# Hex dump first record
xxd graph.bin | head -4
```

---

## Choosing an adapter

| | `JsonFileStorage` | `BinaryFileStorage` |
|--|--|--|
| Human readable | ✓ | ✗ |
| File size | 2–3× larger | Compact |
| Debuggability | High | Low (need hex editor) |
| Parse speed | Slower | Faster |
| Property type safety | Via serde | Via hand-written codec |
| Educational value | Shows serialization via serde | Shows low-level binary encoding |

Both implement `StoragePort` identically. The rest of the database
(cache, engine, algorithms, query) cannot tell the difference.

---

## Adding a new storage format

See [09_adding_adapters.md](09_adding_adapters.md) for a step-by-step guide.
Ideas:
- **Parquet** — columnar format, great for analytics queries
- **SQLite** — use SQLite as a storage backend (ironic but educational)
- **RocksDB** — LSM-tree based, excellent for write-heavy workloads
- **Memory-mapped file** — use `mmap` for zero-copy reads
