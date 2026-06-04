// SharedDatabase — thread-safe wrapper around LayeredGraphDatabase.
//
// LayeredGraphDatabase is single-threaded by design: its methods take `&mut self`
// because the LRU cache updates on every read.  To share it across threads we
// wrap it in Arc<Mutex<>>.
//
// ── Concurrency model ─────────────────────────────────────────────────────────
//
//   Every query (read or write) holds the Mutex for its duration.
//   Threads are serialized at the database level — one query at a time.
//   This is correct and easy to reason about; throughput is limited by the
//   number of cores but latency is bounded by a single query's duration.
//
//   For higher concurrency, the next steps are:
//   1. Split the Mutex into two: RwLock for read-only queries + Mutex for writes.
//      Requires making the cache use interior mutability (Mutex<HashMap>) so
//      `get_node` can take `&self` instead of `&mut self`.
//   2. MVCC: readers see a consistent snapshot; writers append new versions.
//      See docs/17_concurrency_and_safety.md for detailed designs.
//
// ── Usage ─────────────────────────────────────────────────────────────────────
//
//   let db    = LayeredGraphDatabase::open(...)?;
//   let shared = SharedDatabase::new(db);
//
//   // Clone a handle for each thread/connection:
//   let handle = shared.clone_handle();
//   std::thread::spawn(move || {
//       let result = handle.execute_query(&SimpleQueryLanguage, "COUNT NODES").unwrap();
//       println!("{result}");
//   });

use std::sync::{Arc, Mutex, MutexGuard};

use crate::core::{
    error::GraphError,
    node::{Node, NodeId},
    value::Value,
};
use crate::database::{
    layered::LayeredGraphDatabase,
    metrics::DatabaseMetrics,
};
use crate::query::{port::QueryLanguagePort, result::QueryResult};
use std::collections::HashMap;

// ── SharedDatabase ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SharedDatabase {
    inner: Arc<Mutex<LayeredGraphDatabase>>,
}

impl SharedDatabase {
    pub fn new(db: LayeredGraphDatabase) -> Self {
        Self { inner: Arc::new(Mutex::new(db)) }
    }

    /// Clone a handle.  All clones share the same underlying database.
    pub fn clone_handle(&self) -> Self { self.clone() }

    /// Lock the database and return a guard.
    /// The guard releases the lock when dropped.
    pub fn lock(&self) -> MutexGuard<'_, LayeredGraphDatabase> {
        self.inner.lock().expect("database mutex poisoned")
    }

    // ── Convenience pass-through methods ─────────────────────────────────────
    // These acquire the lock, perform the operation, and release immediately.

    pub fn execute_query(
        &self,
        language: &dyn QueryLanguagePort,
        query:    &str,
    ) -> Result<QueryResult, GraphError> {
        self.lock().execute_query(language, query)
    }

    pub fn insert_node(
        &self,
        label:      &str,
        properties: HashMap<String, Value>,
    ) -> Result<NodeId, GraphError> {
        self.lock().insert_node(label, properties)
    }

    pub fn get_node(&self, id: NodeId) -> Result<Option<Node>, GraphError> {
        self.lock().get_node(id)
    }

    pub fn node_count(&self) -> usize { self.lock().node_count() }
    pub fn edge_count(&self) -> usize { self.lock().edge_count() }

    pub fn metrics(&self) -> DatabaseMetrics { self.lock().metrics().clone() }

    pub fn compact(&self) -> Result<(), GraphError> {
        self.lock().compact()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use crate::adapters::{
        cache::no_cache::NoCache,
        engine::adjacency_list::AdjacencyListEngine,
        storage::json_file::JsonFileStorage,
    };
    use crate::database::config::DatabaseConfig;
    use crate::query::languages::simple::SimpleQueryLanguage;
    use crate::test_helpers::TempPath;

    fn make_shared(path: &std::path::PathBuf) -> SharedDatabase {
        let db = LayeredGraphDatabase::open_with_config(
            Box::new(JsonFileStorage::open(path).unwrap()),
            Box::new(NoCache),
            Box::new(AdjacencyListEngine::new()),
            DatabaseConfig::unrestricted(),
        ).unwrap();
        SharedDatabase::new(db)
    }

    #[test]
    fn clone_handle_shares_state() {
        let tmp = TempPath::new("shared_test.json");
        let shared = make_shared(tmp.path());
        shared.insert_node("City", HashMap::new()).unwrap();

        let handle2 = shared.clone_handle();
        assert_eq!(handle2.node_count(), 1);
    }

    #[test]
    fn concurrent_reads_from_multiple_threads() {
        let tmp = TempPath::new("shared_threads.json");
        let shared = make_shared(tmp.path());
        shared.insert_node("City", HashMap::new()).unwrap();

        let handles: Vec<_> = (0..4).map(|_| {
            let db = shared.clone_handle();
            thread::spawn(move || {
                let result = db.execute_query(
                    &SimpleQueryLanguage,
                    "COUNT NODES",
                ).unwrap();
                // All threads should see Count(1).
                matches!(result, QueryResult::Count(1))
            })
        }).collect();

        for h in handles {
            assert!(h.join().unwrap());
        }
    }
}
