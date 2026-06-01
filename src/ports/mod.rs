// Ports — the interfaces that the core application depends on.
//
// "Port" is the hexagonal architecture term for a boundary interface.
// The core never imports concrete adapters; it only knows these traits.
// Swap an adapter by providing a different implementation of the trait.

pub mod algorithm;
pub mod cache;
pub mod engine;
pub mod query_context;
pub mod storage;
