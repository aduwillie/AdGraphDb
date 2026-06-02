// Query subsystem — pluggable query language execution.
//
// The three-layer design:
//
//   Language adapter (languages/)
//     Parses a DSL string into the QueryCommand IR.
//     Implements QueryLanguagePort.
//
//   IR (ast.rs)
//     Language-neutral representation of what the query wants.
//     Both parsers produce this; the executor consumes it.
//
//   Executor (executor.rs)
//     Maps QueryCommand variants to DatabaseContext calls.
//     Shared by all language adapters.
//
// Adding a new language requires only a new parser in languages/ — no changes
// to the executor, IR, or database.

pub mod ast;
pub mod executor;
pub mod languages;
pub mod planner;
pub mod port;
pub mod result;
