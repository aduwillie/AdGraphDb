// Adapters — concrete implementations of the ports defined in ports/.
//
// Each sub-module can be compiled in, left out, or replaced independently.
// The application core never imports these directly; it only holds
// Box<dyn Port> trait objects.

pub mod cache;
pub mod engine;
pub mod index;
pub mod storage;
