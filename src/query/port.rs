// QueryLanguagePort — the interface every query language adapter implements.
//
// To add a new query language (e.g. GraphQL, SPARQL, a custom DSL):
//   1. Create a file in query/languages/
//   2. Implement QueryLanguagePort for your struct
//   3. Pass it to LayeredGraphDatabase::execute_query()
//
// The single required method takes a raw query string, parses it into the
// shared QueryCommand IR, and executes it via the shared executor.  The
// database and executor are shared; only the parser differs.

use crate::core::error::GraphError;
use crate::ports::query_context::DatabaseContext;
use crate::query::result::QueryResult;

pub trait QueryLanguagePort {
    /// Human-readable name shown in error messages and logs.
    fn language_name(&self) -> &str;

    /// Parse `query` and execute it against `context`.
    fn execute(
        &self,
        query: &str,
        context: &mut dyn DatabaseContext,
    ) -> Result<QueryResult, GraphError>;
}
