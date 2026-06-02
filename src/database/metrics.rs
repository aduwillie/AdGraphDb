// DatabaseMetrics — runtime counters for observability and tuning.
//
// All counters are simple u64 values — no atomics, no locks.
// The database is single-threaded, so plain integers are safe.
//
// Retrieve with `db.metrics()` and reset with `db.reset_metrics()`.

use std::time::{Duration, Instant};

/// Accumulated runtime statistics for one database instance.
#[derive(Debug, Default, Clone)]
pub struct DatabaseMetrics {
    // ── Query counters ────────────────────────────────────────────────────────
    pub queries_executed:      u64,
    pub total_query_time_ns:   u64,

    // ── Index usage ───────────────────────────────────────────────────────────
    pub label_index_hits:      u64,   // times label index was used instead of full scan
    pub property_index_hits:   u64,   // times property index was used
    pub full_node_scans:       u64,   // times a full node scan was needed
    pub full_edge_scans:       u64,

    // ── Cache ─────────────────────────────────────────────────────────────────
    pub cache_node_hits:       u64,
    pub cache_node_misses:     u64,
    pub cache_edge_hits:       u64,
    pub cache_edge_misses:     u64,

    // ── Storage writes ────────────────────────────────────────────────────────
    pub nodes_inserted:        u64,
    pub nodes_deleted:         u64,
    pub edges_inserted:        u64,
    pub edges_deleted:         u64,
    pub compactions:           u64,
    pub transactions_committed: u64,
    pub transactions_rolled_back: u64,
}

impl DatabaseMetrics {
    pub fn new() -> Self { Self::default() }

    // ── Derived metrics ───────────────────────────────────────────────────────

    pub fn cache_node_hit_rate(&self) -> f64 {
        let total = self.cache_node_hits + self.cache_node_misses;
        if total == 0 { 0.0 } else { self.cache_node_hits as f64 / total as f64 }
    }

    pub fn cache_edge_hit_rate(&self) -> f64 {
        let total = self.cache_edge_hits + self.cache_edge_misses;
        if total == 0 { 0.0 } else { self.cache_edge_hits as f64 / total as f64 }
    }

    pub fn avg_query_ms(&self) -> f64 {
        if self.queries_executed == 0 {
            0.0
        } else {
            self.total_query_time_ns as f64 / self.queries_executed as f64 / 1_000_000.0
        }
    }

    pub fn index_hit_rate(&self) -> f64 {
        let indexed = self.label_index_hits + self.property_index_hits;
        let total   = indexed + self.full_node_scans;
        if total == 0 { 0.0 } else { indexed as f64 / total as f64 }
    }

    pub fn total_writes(&self) -> u64 {
        self.nodes_inserted + self.nodes_deleted + self.edges_inserted + self.edges_deleted
    }
}

impl std::fmt::Display for DatabaseMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "─── DatabaseMetrics ───────────────────────────")?;
        writeln!(f, "  Queries          : {}", self.queries_executed)?;
        writeln!(f, "  Avg query time   : {:.2} ms", self.avg_query_ms())?;
        writeln!(f, "  Node cache hit%  : {:.1}%", self.cache_node_hit_rate() * 100.0)?;
        writeln!(f, "  Edge cache hit%  : {:.1}%", self.cache_edge_hit_rate() * 100.0)?;
        writeln!(f, "  Index hit%       : {:.1}%", self.index_hit_rate() * 100.0)?;
        writeln!(f, "    label hits     : {}", self.label_index_hits)?;
        writeln!(f, "    property hits  : {}", self.property_index_hits)?;
        writeln!(f, "    full scans     : {}", self.full_node_scans)?;
        writeln!(f, "  Writes           : {}", self.total_writes())?;
        writeln!(f, "  Compactions      : {}", self.compactions)?;
        writeln!(f, "  Txns committed   : {}", self.transactions_committed)?;
        write!  (f, "  Txns rolled back : {}", self.transactions_rolled_back)
    }
}

// ── Query timing RAII guard ───────────────────────────────────────────────────
//
// Usage:
//   let _t = QueryTimer::start(&mut metrics);   // starts timing
//   // ... run query ...
//   // _t dropped here → records elapsed time

pub struct QueryTimer<'a> {
    metrics: &'a mut DatabaseMetrics,
    start:   Instant,
}

impl<'a> QueryTimer<'a> {
    pub fn start(metrics: &'a mut DatabaseMetrics) -> Self {
        Self { metrics, start: Instant::now() }
    }
}

impl Drop for QueryTimer<'_> {
    fn drop(&mut self) {
        let elapsed: Duration = self.start.elapsed();
        self.metrics.queries_executed    += 1;
        self.metrics.total_query_time_ns += elapsed.as_nanos() as u64;
    }
}
