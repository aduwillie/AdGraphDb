// Executor — runs an ExecutionPlan (produced by QueryPlanner) against a
// DatabaseContext.
//
// Public entry points:
//   execute()                — plan + execute in one call (normal path)
//   execute_with_explain()   — plan + execute, returns plan too (EXPLAIN / logging)
//   execute_plan()           — run a pre-computed plan

use crate::core::error::GraphError;
use crate::ports::query_context::DatabaseContext;
use crate::query::{
    ast::{QueryCommand, TraversalKind},
    planner::{ExecutionPlan, QueryPlanner},
    result::QueryResult,
};

// ── Public API ────────────────────────────────────────────────────────────────

/// Plan and execute a query in one step.
pub fn execute(
    command: QueryCommand,
    ctx: &mut dyn DatabaseContext,
) -> Result<QueryResult, GraphError> {
    let stats = ctx.stats();
    let plan  = QueryPlanner::plan(command, &stats);
    execute_plan(plan, ctx)
}

/// Plan and execute, returning the chosen plan alongside the result.
/// Use this to log slow queries with their plan or to implement EXPLAIN.
pub fn execute_with_explain(
    command: QueryCommand,
    ctx: &mut dyn DatabaseContext,
) -> Result<(QueryResult, ExecutionPlan), GraphError> {
    let stats = ctx.stats();
    let plan  = QueryPlanner::plan(command, &stats);
    let result = execute_plan(plan.clone(), ctx)?;
    Ok((result, plan))
}

/// Run a pre-computed plan directly (skips the planner).
pub fn execute_plan(
    plan: ExecutionPlan,
    ctx: &mut dyn DatabaseContext,
) -> Result<QueryResult, GraphError> {
    match plan {
        // ── Node match strategies ─────────────────────────────────────────────

        ExecutionPlan::PropertyIndexScan { field, op, value, remaining_filter } => {
            let candidates = ctx.get_nodes_by_property(&field, &op, &value)?;
            let matched: Vec<_> = candidates
                .into_iter()
                .filter(|n| remaining_filter.matches(n))
                .collect();
            Ok(QueryResult::Nodes(matched))
        }

        ExecutionPlan::LabelIndexScan { label: _, remaining_filter } => {
            let label = remaining_filter.label.as_deref().unwrap_or("");
            let candidates = ctx.get_nodes_by_label(label)?;
            let matched: Vec<_> = candidates
                .into_iter()
                .filter(|n| remaining_filter.matches(n))
                .collect();
            Ok(QueryResult::Nodes(matched))
        }

        ExecutionPlan::FullNodeScan { filter } => {
            let all = ctx.get_all_nodes()?;
            let matched: Vec<_> = all.into_iter().filter(|n| filter.matches(n)).collect();
            Ok(QueryResult::Nodes(matched))
        }

        // ── Edge strategies ───────────────────────────────────────────────────

        ExecutionPlan::FullEdgeScan { filter } => {
            let all = ctx.get_all_edges()?;
            let matched: Vec<_> = all.into_iter().filter(|e| filter.matches(e)).collect();
            Ok(QueryResult::Edges(matched))
        }

        // ── Point lookups ─────────────────────────────────────────────────────

        ExecutionPlan::NodeLookup { id } =>
            Ok(QueryResult::SingleNode(ctx.get_node(id)?)),

        ExecutionPlan::EdgeLookup { id } =>
            Ok(QueryResult::SingleEdge(ctx.get_edge(id)?)),

        // ── Traversals ────────────────────────────────────────────────────────

        ExecutionPlan::Traverse { kind, start } => {
            let ids = match kind {
                TraversalKind::Bfs => ctx.traverse_bfs(start),
                TraversalKind::Dfs => ctx.traverse_dfs(start),
            };
            Ok(QueryResult::Traversal(ids))
        }

        // ── Shortest path ─────────────────────────────────────────────────────

        ExecutionPlan::ShortestPath { start, goal } =>
            match ctx.shortest_path_dijkstra(start, goal) {
                Some((nodes, weight)) => Ok(QueryResult::Path { nodes, total_weight: weight }),
                None                  => Ok(QueryResult::Empty),
            },

        // ── Counts ────────────────────────────────────────────────────────────

        ExecutionPlan::CountNodes => Ok(QueryResult::Count(ctx.node_count())),
        ExecutionPlan::CountEdges => Ok(QueryResult::Count(ctx.edge_count())),
    }
}
