// Query language adapters.
//
// Each module is a self-contained parser that produces the shared QueryCommand IR.
// Add a new language by adding a module here and implementing QueryLanguagePort.

pub mod cypher_lite;
pub mod simple;
