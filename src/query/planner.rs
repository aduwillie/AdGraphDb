// QueryPlanner — cost-based optimizer that converts a QueryCommand into an
// ExecutionPlan before the executor runs it.
//
// ── Why a planner? ─────────────────────────────────────────────────────────────
//
//   A QueryCommand records *intent* ("find all City nodes with population > 1M").
//   An ExecutionPlan records *strategy* ("use the property index on 'population',
//   then filter by label").  The planner bridges the two using statistics about
//   available indexes and data distribution.
//
//   Without a planner, the executor always does the same thing regardless of
//   what indexes exist.  With a planner, it chooses the cheapest available path.
//
// ── Cost model ─────────────────────────────────────────────────────────────────
//
//   Full scan:          cost = N  (N = total nodes)
//   Label index scan:   cost = label_count  (typically << N)
//   Property index scan: cost = log2(N) + result_count  (BTree seek + range walk)
//
//   The planner picks the strategy with the lowest estimated cost.
//   When multiple strategies tie, property index beats label beats full scan
//   (property indexes are more selective on average).
//
// ── Extending the planner ──────────────────────────────────────────────────────
//
//   1. Add a new ExecutionPlan variant for your strategy.
//   2. Add a cost estimate in `estimated_cost()`.
//   3. Add dispatch in `executor::execute_plan()`.
//   4. No other files need to change.

use std::collections::{HashMap, HashSet};

use crate::query::ast::{
    ComparisonOp, EdgeFilter, NodeFilter, PropertyCondition, QueryCommand, TraversalKind,
};
use crate::core::{edge::EdgeId, node::NodeId, value::Value};

// ── Database statistics (input to the planner) ────────────────────────────────

/// Snapshot of database shape and index availability used by the planner.
/// Obtained cheaply from `LayeredGraphDatabase::stats()`.
#[derive(Debug, Clone)]
pub struct DatabaseStats {
    pub node_count:              usize,
    pub edge_count:              usize,
    /// How many nodes carry each label.
    pub label_counts:            HashMap<String, usize>,
    /// Node property fields that have a PropertyIndex (support O(log N) queries).
    pub indexed_node_fields:     HashSet<String>,
}

impl DatabaseStats {
    pub fn empty() -> Self {
        Self {
            node_count:          0,
            edge_count:          0,
            label_counts:        HashMap::new(),
            indexed_node_fields: HashSet::new(),
        }
    }

    fn label_count(&self, label: &str) -> usize {
        self.label_counts.get(label).copied().unwrap_or(0)
    }
}

// ── Execution plan ─────────────────────────────────────────────────────────────

/// The strategy the executor will use to run a query.
/// Each variant corresponds to one code path in `executor::execute_plan`.
#[derive(Debug, Clone)]
pub enum ExecutionPlan {
    // ── Node match strategies (ordered from cheapest to most expensive) ───────

    /// Use the PropertyIndex on `field` to find candidates, then apply the
    /// remaining node filter conditions.
    ///
    /// Cost: O(log N + property_matches)
    PropertyIndexScan {
        field:              String,
        op:                 ComparisonOp,
        value:              Value,
        remaining_filter:   NodeFilter,
    },

    /// Use the LabelIndex to find candidates for `label`, then apply any
    /// remaining property conditions.
    ///
    /// Cost: O(label_count + label_count × remaining_conditions)
    LabelIndexScan {
        label:              String,
        remaining_filter:   NodeFilter,
    },

    /// Load and check every node.
    ///
    /// Cost: O(N)
    FullNodeScan { filter: NodeFilter },

    // ── Edge match strategies ─────────────────────────────────────────────────
    FullEdgeScan { filter: EdgeFilter },

    // ── Already-optimal (no planning benefit) ─────────────────────────────────
    NodeLookup     { id: NodeId },
    EdgeLookup     { id: EdgeId },
    Traverse       { kind: TraversalKind, start: NodeId },
    ShortestPath   { start: NodeId, goal: NodeId },
    CountNodes,
    CountEdges,
}

impl ExecutionPlan {
    /// Human-readable description of this plan, including its estimated cost.
    pub fn describe(&self, stats: &DatabaseStats) -> String {
        match self {
            ExecutionPlan::PropertyIndexScan { field, op, value, .. } => {
                let cost = estimated_property_index_cost(stats);
                format!("PropertyIndexScan({field} {op} {value})  est. cost ~{cost}")
            }
            ExecutionPlan::LabelIndexScan { label, .. } => {
                let count = stats.label_count(label);
                format!("LabelIndexScan(label={label})  est. cost ~{count}")
            }
            ExecutionPlan::FullNodeScan { .. } => {
                format!("FullNodeScan  est. cost ~{}", stats.node_count)
            }
            ExecutionPlan::FullEdgeScan { .. } => {
                format!("FullEdgeScan  est. cost ~{}", stats.edge_count)
            }
            ExecutionPlan::NodeLookup { id }     => format!("NodeLookup({id})  cost O(1)"),
            ExecutionPlan::EdgeLookup { id }     => format!("EdgeLookup({id})  cost O(1)"),
            ExecutionPlan::Traverse { kind, start } =>
                format!("Traverse({kind:?}, start={start})  cost O(reachable)"),
            ExecutionPlan::ShortestPath { start, goal } =>
                format!("ShortestPath({start}→{goal})  cost O((V+E)logV)"),
            ExecutionPlan::CountNodes => "CountNodes  cost O(1)".into(),
            ExecutionPlan::CountEdges => "CountEdges  cost O(1)".into(),
        }
    }
}

// ── QueryPlanner ──────────────────────────────────────────────────────────────

pub struct QueryPlanner;

impl QueryPlanner {
    /// Convert a `QueryCommand` into the cheapest available `ExecutionPlan`
    /// given the current `DatabaseStats`.
    pub fn plan(command: QueryCommand, stats: &DatabaseStats) -> ExecutionPlan {
        match command {
            QueryCommand::MatchNodes(filter) =>
                Self::plan_match_nodes(filter, stats),

            QueryCommand::MatchEdges(filter) =>
                ExecutionPlan::FullEdgeScan { filter },

            // These are already O(1) — no planning benefit.
            QueryCommand::GetNode(id)    => ExecutionPlan::NodeLookup { id },
            QueryCommand::GetEdge(id)    => ExecutionPlan::EdgeLookup { id },
            QueryCommand::Traverse { kind, start } =>
                ExecutionPlan::Traverse { kind, start },
            QueryCommand::ShortestPath { start, goal } =>
                ExecutionPlan::ShortestPath { start, goal },
            QueryCommand::CountNodes => ExecutionPlan::CountNodes,
            QueryCommand::CountEdges => ExecutionPlan::CountEdges,
        }
    }

    fn plan_match_nodes(filter: NodeFilter, stats: &DatabaseStats) -> ExecutionPlan {
        let full_scan_cost = stats.node_count;

        // ── Option A: property index ──────────────────────────────────────────
        //
        // If any property condition targets an indexed field, use the property
        // index for that condition and apply the rest as post-filters.
        // Pick the condition estimated to be most selective.
        if let Some(best_cond) = best_indexed_condition(&filter.property_conditions, stats) {
            let property_cost = estimated_property_index_cost(stats);
            let label_cost    = filter.label.as_ref()
                .map(|l| stats.label_count(l))
                .unwrap_or(full_scan_cost);

            if property_cost <= label_cost && property_cost < full_scan_cost {
                // Build a remaining filter with this condition removed.
                let mut remaining = filter.clone();
                remaining.property_conditions
                    .retain(|c| c.key != best_cond.key || c.op != best_cond.op || c.value != best_cond.value);

                return ExecutionPlan::PropertyIndexScan {
                    field:            best_cond.key.clone(),
                    op:               best_cond.op.clone(),
                    value:            best_cond.value.clone(),
                    remaining_filter: remaining,
                };
            }
        }

        // ── Option B: label index ─────────────────────────────────────────────
        if let Some(ref label) = filter.label {
            let label_cost = stats.label_count(label);
            if label_cost < full_scan_cost {
                // Label filter is consumed by the index; remaining filter checks
                // only property conditions (label check is redundant but harmless).
                return ExecutionPlan::LabelIndexScan {
                    label:            label.clone(),
                    remaining_filter: filter,
                };
            }
        }

        // ── Option C: full scan ───────────────────────────────────────────────
        ExecutionPlan::FullNodeScan { filter }
    }
}

// ── Cost helpers ──────────────────────────────────────────────────────────────

fn estimated_property_index_cost(stats: &DatabaseStats) -> usize {
    // O(log N) seek + assume 10% selectivity (rough heuristic).
    if stats.node_count == 0 {
        return 0;
    }
    let log_n = (stats.node_count as f64).log2().ceil() as usize;
    let tenth = stats.node_count / 10;
    log_n + tenth
}

/// Find the property condition most likely to be served by an index and
/// be selective.  Returns None if no condition targets an indexed field.
fn best_indexed_condition<'a>(
    conditions: &'a [PropertyCondition],
    stats: &DatabaseStats,
) -> Option<&'a PropertyCondition> {
    conditions
        .iter()
        .filter(|c| stats.indexed_node_fields.contains(&c.key))
        // Prefer equality (most selective) then range ops
        .min_by_key(|c| op_selectivity_rank(&c.op))
}

/// Lower rank = more selective = preferred.
fn op_selectivity_rank(op: &ComparisonOp) -> u8 {
    match op {
        ComparisonOp::Eq    => 0,
        ComparisonOp::NotEq => 4,
        ComparisonOp::Lt | ComparisonOp::LtEq => 2,
        ComparisonOp::Gt | ComparisonOp::GtEq => 2,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{NodeFilter, PropertyCondition, QueryCommand, TraversalKind};
    use crate::core::{node::NodeId, value::Value};

    fn stats_with_label(label: &str, count: usize, total: usize) -> DatabaseStats {
        let mut s = DatabaseStats::empty();
        s.node_count = total;
        s.label_counts.insert(label.into(), count);
        s
    }

    #[test]
    fn empty_filter_produces_full_scan() {
        let stats = DatabaseStats::empty();
        let cmd = QueryCommand::MatchNodes(NodeFilter::default());
        assert!(matches!(QueryPlanner::plan(cmd, &stats), ExecutionPlan::FullNodeScan { .. }));
    }

    #[test]
    fn label_filter_uses_label_index_when_selective() {
        let stats = stats_with_label("City", 10, 1_000);
        let mut filter = NodeFilter::default();
        filter.label = Some("City".into());
        let plan = QueryPlanner::plan(QueryCommand::MatchNodes(filter), &stats);
        assert!(matches!(plan, ExecutionPlan::LabelIndexScan { .. }));
    }

    #[test]
    fn label_filter_falls_back_to_full_scan_when_all_nodes_are_that_label() {
        // If every node is a City, the label index gives no benefit.
        let stats = stats_with_label("City", 1_000, 1_000);
        let mut filter = NodeFilter::default();
        filter.label = Some("City".into());
        let plan = QueryPlanner::plan(QueryCommand::MatchNodes(filter), &stats);
        assert!(matches!(plan, ExecutionPlan::FullNodeScan { .. }));
    }

    #[test]
    fn property_index_preferred_over_label_when_more_selective() {
        let mut stats = stats_with_label("City", 500, 1_000);
        stats.indexed_node_fields.insert("population".into());
        // label_cost = 500, property_cost = log2(1000) + 100 ≈ 110 < 500
        let mut filter = NodeFilter::default();
        filter.label = Some("City".into());
        filter.property_conditions.push(PropertyCondition {
            key:   "population".into(),
            op:    ComparisonOp::Gt,
            value: Value::Integer(1_000_000),
        });
        let plan = QueryPlanner::plan(QueryCommand::MatchNodes(filter), &stats);
        assert!(matches!(plan, ExecutionPlan::PropertyIndexScan { .. }));
    }

    #[test]
    fn get_node_produces_lookup() {
        let plan = QueryPlanner::plan(
            QueryCommand::GetNode(NodeId(5)),
            &DatabaseStats::empty(),
        );
        assert!(matches!(plan, ExecutionPlan::NodeLookup { .. }));
    }

    #[test]
    fn traverse_passthrough() {
        let plan = QueryPlanner::plan(
            QueryCommand::Traverse { kind: TraversalKind::Bfs, start: NodeId(0) },
            &DatabaseStats::empty(),
        );
        assert!(matches!(plan, ExecutionPlan::Traverse { .. }));
    }

    #[test]
    fn count_passthrough() {
        let plan = QueryPlanner::plan(QueryCommand::CountNodes, &DatabaseStats::empty());
        assert!(matches!(plan, ExecutionPlan::CountNodes));
    }
}
