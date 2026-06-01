// test_helpers — shared utilities for unit and integration tests.
//
// NOT compiled into production builds (cfg(test) at each call site).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns a unique temp-file path that will not collide across parallel tests.
/// The caller is responsible for deleting the file (use TempPath::new).
pub fn unique_temp_path(suffix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("adgraphdb_test_{n}_{suffix}"))
}

/// RAII guard: deletes the file when dropped.
pub struct TempPath(pub PathBuf);

impl TempPath {
    pub fn new(suffix: &str) -> Self {
        Self(unique_temp_path(suffix))
    }

    pub fn path(&self) -> &PathBuf { &self.0 }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // Also clean up any .tmp companion files left by compact().
        let tmp = self.0.with_extension(
            self.0.extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("{e}.tmp"))
                .unwrap_or_else(|| "tmp".into()),
        );
        let _ = std::fs::remove_file(tmp);
    }
}

/// Build a minimal LayeredGraphDatabase backed by JSON storage for tests.
#[cfg(test)]
pub fn json_test_db(
    path: &PathBuf,
) -> crate::database::layered::LayeredGraphDatabase {
    use crate::{
        adapters::{
            cache::no_cache::NoCache,
            engine::adjacency_list::AdjacencyListEngine,
            storage::json_file::JsonFileStorage,
        },
        database::layered::LayeredGraphDatabase,
    };
    let storage = JsonFileStorage::open(path).unwrap();
    LayeredGraphDatabase::open(
        Box::new(storage),
        Box::new(NoCache),
        Box::new(AdjacencyListEngine::new()),
    )
    .unwrap()
}

/// Build a minimal LayeredGraphDatabase backed by binary storage for tests.
#[cfg(test)]
pub fn binary_test_db(
    path: &PathBuf,
) -> crate::database::layered::LayeredGraphDatabase {
    use crate::{
        adapters::{
            cache::no_cache::NoCache,
            engine::adjacency_list::AdjacencyListEngine,
            storage::binary_file::BinaryFileStorage,
        },
        database::layered::LayeredGraphDatabase,
    };
    let storage = BinaryFileStorage::open(path).unwrap();
    LayeredGraphDatabase::open(
        Box::new(storage),
        Box::new(NoCache),
        Box::new(AdjacencyListEngine::new()),
    )
    .unwrap()
}
