// Index adapters — secondary indexes that make filter queries faster.
//
// These are RAM-only structures, always derived from the WAL at startup.
// They do not implement a port — they are optimization details held
// directly inside LayeredGraphDatabase.
//
// Current indexes:
//   LabelIndex — maps label string → Vec<NodeId>
//                converts O(N) label scans to O(1) lookups

pub mod label_index;
pub mod property_index;
