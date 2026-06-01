// AdGraphDb — an educational graph database
//
// Architecture: Ports & Adapters (Hexagonal)
//
//   ┌─────────────────────────────────────────────────────┐
//   │                    Application                      │
//   │              (LayeredGraphDatabase)                 │
//   │                                                     │
//   │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐  │
//   │  │  Engine  │  │  Cache   │  │    Storage       │  │
//   │  │   Port   │  │   Port   │  │     Port         │  │
//   │  └────┬─────┘  └────┬─────┘  └────────┬─────────┘  │
//   └───────┼─────────────┼─────────────────┼────────────┘
//           │             │                 │
//   ┌───────▼─────┐ ┌─────▼──────┐ ┌───────▼──────────┐
//   │ Adjacency   │ │ LruCache / │ │ JsonFile /       │
//   │ ListEngine  │ │ NoCache    │ │ BinaryFile       │
//   └─────────────┘ └────────────┘ └──────────────────┘

pub mod algorithms;
pub mod adapters;
pub mod core;
pub mod database;
pub mod ports;
pub mod query;
pub mod transaction;

#[cfg(test)]
pub mod test_helpers;
