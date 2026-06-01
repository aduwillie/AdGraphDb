// Executor — runs a QueryCommand against a DatabaseContext.
//
// This is the single place where the IR (QueryCommand) is interpreted.
// Every query language parser produces a QueryCommand; this function
// executes it.  No parser imports this directly — they call it via
// the shared helper in their QueryLanguagePort implementation.
//
// Pattern: Command + Invoker
//   QueryCommand  = Command (carries intent)
//   execute()     = Invoker (dispatches to receiver)
//   DatabaseContext = Receiver (performs the actual work)

use crate::core::error::GraphError;
use crate::ports::query_context::DatabaseContext;
use crate::query::{
    ast::{QueryCommand, TraversalKind},
    result::QueryResult,
};

pub fn execute(
    command: QueryCommand,
    ctx: &mut dyn DatabaseContext,
) -> Result<QueryResult, GraphError> {
    match command {
        // ── Structural matches ────────────────────────────────────────────────

        QueryCommand::MatchNodes(filter) => {
            // Fast path: if the filter names a label, use the label index
            // to fetch only the matching candidate set — O(label_count).
            // Slow path: no label specified → full scan — O(N).
            let candidates = match &filter.label {
                Some(label) => ctx.get_nodes_by_label(label)?,
                None        => ctx.get_all_nodes()?,
            };
            // Apply any remaining property conditions across the candidate set.
            let matched: Vec<_> = candidates
                .into_iter()
                .filter(|n| filter.matches(n))
                .collect();
            Ok(QueryResult::Nodes(matched))
        }

        QueryCommand::MatchEdges(filter) => {
            let all = ctx.get_all_edges()?;
            let matched: Vec<_> = all.into_iter().filter(|e| filter.matches(e)).collect();
            Ok(QueryResult::Edges(matched))
        }

        // ── Point lookups ─────────────────────────────────────────────────────

        QueryCommand::GetNode(id) => {
            Ok(QueryResult::SingleNode(ctx.get_node(id)?))
        }

        QueryCommand::GetEdge(id) => {
            Ok(QueryResult::SingleEdge(ctx.get_edge(id)?))
        }

        // ── Traversals ────────────────────────────────────────────────────────

        QueryCommand::Traverse { kind, start } => {
            let ids = match kind {
                TraversalKind::Bfs => ctx.traverse_bfs(start),
                TraversalKind::Dfs => ctx.traverse_dfs(start),
            };
            Ok(QueryResult::Traversal(ids))
        }

        // ── Shortest path ─────────────────────────────────────────────────────

        QueryCommand::ShortestPath { start, goal } => {
            match ctx.shortest_path_dijkstra(start, goal) {
                Some((nodes, total_weight)) => Ok(QueryResult::Path { nodes, total_weight }),
                None => Ok(QueryResult::Empty),
            }
        }

        // ── Counts ────────────────────────────────────────────────────────────

        QueryCommand::CountNodes => Ok(QueryResult::Count(ctx.node_count())),
        QueryCommand::CountEdges => Ok(QueryResult::Count(ctx.edge_count())),
    }
}
