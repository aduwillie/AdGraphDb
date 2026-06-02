// DatabaseConfig — all tuning parameters for LayeredGraphDatabase.
//
// Pass a custom config to `LayeredGraphDatabase::open_with_config`.
// `DatabaseConfig::default()` provides conservative production-ready values.

/// Configuration for a `LayeredGraphDatabase` instance.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    // ── Auto-compaction ───────────────────────────────────────────────────────
    /// Trigger an automatic WAL compaction after this many write operations
    /// (insert_node, delete_node, insert_edge, delete_edge).
    /// `None` disables automatic compaction; call `db.compact()` manually.
    pub auto_compact_after_writes: Option<usize>,

    // ── Query safety ──────────────────────────────────────────────────────────
    /// Return an error if a full node scan would touch more than this many
    /// nodes.  Protects against accidental O(N) queries on large graphs.
    /// `None` = unlimited (no guard-rail).
    pub max_full_scan_nodes: Option<usize>,

    // ── Slow query logging ────────────────────────────────────────────────────
    /// Print a warning when a query takes longer than this many milliseconds.
    /// `None` = no slow-query logging.
    pub slow_query_warn_ms: Option<u64>,

    // ── Property index ────────────────────────────────────────────────────────
    /// Node property fields to index automatically on insert.
    /// Any field listed here will be added to the PropertyIndex,
    /// making range queries on those fields O(log N + results).
    /// Leave empty to index all fields (default), or list specific ones.
    pub indexed_node_fields: NodeFieldIndexing,
}

/// Which node property fields to auto-index.
#[derive(Debug, Clone)]
pub enum NodeFieldIndexing {
    /// Index every property field seen on any node (default).
    /// Maximises query speed; uses more RAM for high-cardinality graphs.
    All,
    /// Index only these named fields.
    OnlyFields(Vec<String>),
    /// Do not build a property index.  All property filters use full scans.
    None,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            auto_compact_after_writes: Some(10_000),
            max_full_scan_nodes:       None,
            slow_query_warn_ms:        Some(100),
            indexed_node_fields:       NodeFieldIndexing::All,
        }
    }
}

impl DatabaseConfig {
    /// No limits, no auto-compaction, no slow-query warnings.
    /// Useful for tests that need predictable timing and no background work.
    pub fn unrestricted() -> Self {
        Self {
            auto_compact_after_writes: None,
            max_full_scan_nodes:       None,
            slow_query_warn_ms:        None,
            indexed_node_fields:       NodeFieldIndexing::All,
        }
    }

    /// Returns true if `field` should be added to the property index.
    /// Pass `"*"` as a sentinel to check if the config uses All indexing.
    pub fn should_index_field(&self, field: &str) -> bool {
        match &self.indexed_node_fields {
            NodeFieldIndexing::All           => true,
            NodeFieldIndexing::OnlyFields(f) => f.iter().any(|s| s == field),
            NodeFieldIndexing::None          => false,
        }
    }

    /// True if all fields are indexed (regardless of name).
    pub fn indexes_all_fields(&self) -> bool {
        matches!(&self.indexed_node_fields, NodeFieldIndexing::All)
    }
}
