// runbook — scale runbook: loads synthetic data, benchmarks queries, reports results.
//
// Run via the CLI:
//   cargo run --bin cli -- runbook
//   cargo run --bin cli -- runbook --cities 10000 --people 50000 --runs 5
//
// Run as a standalone binary:
//   cargo run --bin runbook
//   cargo run --bin runbook -- --cities 50000
//
// The runbook is entirely self-contained — it creates its own embedded database
// in a temp file, runs all phases, prints results, then cleans up.
// No server needs to be running.

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::{
    adapters::{
        cache::lru::LruCache,
        engine::adjacency_list::AdjacencyListEngine,
        storage::json_file::JsonFileStorage,
    },
    concurrent::SharedDatabase,
    core::value::Value,
    database::{
        config::DatabaseConfig,
        layered::LayeredGraphDatabase,
    },
    query::languages::simple::SimpleQueryLanguage,
    server::GraphServer,
};

// ── Configuration ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RunbookConfig {
    /// Number of City nodes to insert.
    pub city_count:   usize,
    /// Number of Person nodes to insert.
    pub person_count: usize,
    /// How many times to repeat each benchmark query (min/avg/max reported).
    pub bench_runs:   usize,
    /// Number of concurrent client threads for the load-test phase.
    pub concurrency:  usize,
    /// Queries each concurrent client issues during the load test.
    pub load_queries_per_thread: usize,
    /// Print progress within phases.
    pub verbose:      bool,
    /// Path to the temp database file (deleted on completion unless `serve`).
    pub db_path:      String,
    /// After the runbook, keep the data and start the server on this address.
    /// `None` = clean up the temp file and exit.
    pub serve_addr:   Option<String>,
}

impl Default for RunbookConfig {
    fn default() -> Self {
        Self {
            city_count:   10_000,
            person_count: 50_000,
            bench_runs:   3,
            concurrency:  8,
            load_queries_per_thread: 500,
            verbose:      true,
            db_path:      "runbook_temp.json".into(),
            serve_addr:   None,
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(cfg: RunbookConfig) {
    print_banner();

    // ── Phase 0: open database ────────────────────────────────────────────────
    let _ = std::fs::remove_file(&cfg.db_path); // clean slate
    let storage = JsonFileStorage::open(&cfg.db_path)
        .expect("cannot create runbook temp file");

    let db_cfg = DatabaseConfig {
        auto_compact_after_writes: None, // disable during bulk insert
        slow_query_warn_ms: None,
        ..DatabaseConfig::default()
    };

    let mut db = LayeredGraphDatabase::open_with_config(
        Box::new(storage),
        Box::new(LruCache::new(4096, 8192)),
        Box::new(AdjacencyListEngine::new()),
        db_cfg,
    ).expect("cannot open database");

    // ── Phase 1: data ingestion ───────────────────────────────────────────────
    println!("\n{}", section("Phase 1 — Synthetic Data Ingestion"));

    let city_ids   = insert_cities(&mut db, cfg.city_count, cfg.verbose);
    let person_ids = insert_people(&mut db, cfg.person_count, cfg.verbose);
    insert_lives_in_edges(&mut db, &person_ids, &city_ids, cfg.verbose);
    insert_knows_edges(&mut db, &person_ids, cfg.person_count, cfg.verbose);

    // Compact once after all inserts, then warm the cache with one full scan
    // so subsequent benchmark queries hit the cache instead of the WAL.
    let compact_start = Instant::now();
    db.compact().expect("compact failed");
    let wal_kb = db.wal_size_bytes() / 1024;
    println!("  Compacted WAL in {} ms  (WAL size now: {} KB)",
        compact_start.elapsed().as_millis(), wal_kb);

    // Pre-warm the node cache so benchmark timings reflect in-memory speed,
    // not cold-WAL-replay speed.
    print!("  Warming cache ... ");
    io::stdout().flush().ok();
    let _ = db.all_nodes();
    println!("done\n");

    // Reset metrics so Phase 5 reflects only benchmark activity.
    db.reset_metrics();

    // ── Phase 2: query benchmarks ─────────────────────────────────────────────
    println!("{}", section("Phase 2 — Query Benchmarks"));

    let benchmarks: &[(&str, &str)] = &[
        // (description, query)
        ("Label scan: all Cities",
         "MATCH NODE WHERE label = \"City\""),
        ("Label scan: all People",
         "MATCH NODE WHERE label = \"Person\""),
        ("Property index: population > 5 000 000",
         "MATCH NODE WHERE props.population > 5000000"),
        ("Property index: age between 30–40 (≥30 AND label)",
         "MATCH NODE WHERE label = \"Person\" AND props.age > 30"),
        ("Property index: coastal = true",
         "MATCH NODE WHERE props.coastal = true"),
        ("Point lookup: GET NODE N0  (cache cold after compact)",
         "GET NODE N0"),
        ("Point lookup: GET NODE N0  (cache warm)",
         "GET NODE N0"),
        ("Count nodes  O(1)",
         "COUNT NODES"),
        ("Count edges  O(1)",
         "COUNT EDGES"),
        ("BFS from N0  (first 1-hop city neighbourhood)",
         &format!("TRAVERSE BFS FROM N{}", city_ids.first().copied().unwrap_or(crate::core::node::NodeId(0)).0)),
        ("Dijkstra: N0 → N99",
         &format!("PATH FROM N0 TO N{}", city_ids.get(99).copied().unwrap_or(crate::core::node::NodeId(99)).0)),
    ];

    for (desc, query) in benchmarks {
        run_benchmark(&mut db, desc, query, cfg.bench_runs);
    }

    // ── Phase 3: full-scan vs index comparison ────────────────────────────────
    println!("\n{}", section("Phase 3 — Full-Scan vs Index Comparison"));
    compare_scan_vs_index(&mut db, cfg.bench_runs);

    // Snapshot metrics now — Phase 4 resets them for its cold/warm measurement,
    // so we capture the cumulative query/index stats before that happens.
    let query_metrics = db.metrics().clone();

    // ── Phase 4: cache effectiveness ─────────────────────────────────────────
    println!("{}", section("Phase 4 — Cache Effectiveness"));
    cache_effectiveness_report(&mut db);

    // ── Phase 5: metrics ──────────────────────────────────────────────────────
    println!("{}", section("Phase 5 — Runtime Metrics (Phases 2–3)"));
    println!("{}", query_metrics);

    // ── Phase 6: schema ───────────────────────────────────────────────────────
    println!("\n{}", section("Phase 6 — Schema Summary"));
    schema_report(&db);

    // ── Phase 7: concurrent load test (server concurrency model) ───────────────
    //
    // Wrap the same database in a SharedDatabase (Arc<Mutex<>>) — exactly what
    // the TCP server uses — and hammer it from many threads to measure how the
    // mutex-serialized concurrency model holds up under load.
    println!("{}", section("Phase 7 — Concurrent Load Test (server model)"));
    let shared = SharedDatabase::new(db);
    concurrent_load_test(&shared, cfg.concurrency, cfg.load_queries_per_thread);

    // ── Phase 8 (optional): start the server on the loaded data ────────────────
    if let Some(addr) = &cfg.serve_addr {
        println!("{}", section("Phase 8 — Serving Loaded Data"));
        println!("  Synthetic dataset stays loaded and is now served over TCP.");
        println!("  Connect from another terminal:");
        println!("      cargo run --bin cli -- --server {addr}");
        println!("  Press Ctrl-C to stop the server (the temp DB file is kept).\n");

        let server = GraphServer::new(shared, addr.clone());
        if let Err(e) = server.start() {
            eprintln!("  Server error: {e}");
        }
        // Server runs until Ctrl-C; we intentionally do NOT delete the file.
        return;
    }

    // ── Cleanup (only when not serving) ───────────────────────────────────────
    drop(shared);
    let _ = std::fs::remove_file(&cfg.db_path);
    let _ = std::fs::remove_file(format!("{}.tmp", &cfg.db_path));

    println!("\n{}", "=".repeat(62));
    println!("  Runbook complete.");
    println!("{}", "=".repeat(62));
}

// ── Phase 7: concurrent load test ──────────────────────────────────────────────

fn concurrent_load_test(shared: &SharedDatabase, threads: usize, queries_per_thread: usize) {
    use std::thread;

    // A mix of query types representative of real read traffic.
    let query_pool: Vec<String> = vec![
        "MATCH NODE WHERE label = \"City\"".into(),
        "MATCH NODE WHERE props.population > 5000000".into(),
        "COUNT NODES".into(),
        "COUNT EDGES".into(),
        "GET NODE N0".into(),
        "TRAVERSE BFS FROM N0".into(),
    ];

    println!("  {} threads × {} queries each = {} total queries",
        threads, queries_per_thread, fmt_n(threads * queries_per_thread));
    println!("  Query mix: label scan, property index, counts, point lookup, BFS\n");

    let start = Instant::now();

    let handles: Vec<_> = (0..threads).map(|t| {
        let db   = shared.clone_handle();
        let pool = query_pool.clone();
        thread::spawn(move || {
            let lang = SimpleQueryLanguage;
            let mut ok = 0usize;
            for i in 0..queries_per_thread {
                let q = &pool[(t + i) % pool.len()];
                if db.execute_query(&lang, q).is_ok() {
                    ok += 1;
                }
            }
            ok
        })
    }).collect();

    let mut total_ok = 0usize;
    for h in handles {
        total_ok += h.join().unwrap_or(0);
    }

    let elapsed   = start.elapsed();
    let total     = threads * queries_per_thread;
    let secs      = elapsed.as_secs_f64();
    let qps       = if secs > 0.0 { (total_ok as f64 / secs) as usize } else { total_ok };

    println!("  Completed {} / {} queries in {:.2} s",
        fmt_n(total_ok), fmt_n(total), secs);
    println!("  Throughput: {} queries/sec  (across {} threads)", fmt_n(qps), threads);
    println!();
    println!("  Note: queries are serialized by the SharedDatabase mutex — one runs");
    println!("  at a time.  Throughput reflects single-writer concurrency.  See");
    println!("  docs/17_concurrency_and_safety.md for the RwLock/MVCC upgrade path.");
    println!();
}

// ── Data generators ───────────────────────────────────────────────────────────

fn insert_cities(
    db: &mut LayeredGraphDatabase,
    count: usize,
    verbose: bool,
) -> Vec<crate::core::node::NodeId> {
    let countries = ["UK", "France", "Germany", "Spain", "Italy",
                     "Netherlands", "Belgium", "Sweden", "Poland", "Portugal"];
    let start = Instant::now();
    let mut ids = Vec::with_capacity(count);

    for i in 0..count {
        let mut props = HashMap::new();
        props.insert("name".into(),       Value::Text(format!("City{i:06}")));
        props.insert("population".into(), Value::Integer(((i % 10_000) * 1_000 + 50_000) as i64));
        props.insert("country".into(),    Value::Text(countries[i % countries.len()].into()));
        props.insert("coastal".into(),    Value::Boolean(i % 3 == 0));
        props.insert("elevation".into(),  Value::Float((i % 3000) as f64));
        ids.push(db.insert_node("City", props).unwrap());
    }

    let ms = start.elapsed().as_millis();
    let rate = if ms > 0 { (count as u128 * 1000 / ms) as usize } else { count };
    println!("  Cities   {:>8} nodes  {:>6} ms  ({}/sec)",
        fmt_n(count), ms, fmt_n(rate));
    if verbose && count > 0 {
        println!("           first={} last={}", ids[0], ids[count - 1]);
    }
    ids
}

fn insert_people(
    db: &mut LayeredGraphDatabase,
    count: usize,
    verbose: bool,
) -> Vec<crate::core::node::NodeId> {
    let occupations = ["Engineer", "Doctor", "Teacher", "Designer",
                       "Analyst", "Manager", "Researcher", "Artist"];
    let start = Instant::now();
    let mut ids = Vec::with_capacity(count);

    for i in 0..count {
        let mut props = HashMap::new();
        props.insert("name".into(),       Value::Text(format!("Person{i:07}")));
        props.insert("age".into(),        Value::Integer(20 + (i % 60) as i64));
        props.insert("occupation".into(), Value::Text(occupations[i % occupations.len()].into()));
        props.insert("active".into(),     Value::Boolean(i % 5 != 0));
        ids.push(db.insert_node("Person", props).unwrap());
    }

    let ms = start.elapsed().as_millis();
    let rate = if ms > 0 { (count as u128 * 1000 / ms) as usize } else { count };
    println!("  People   {:>8} nodes  {:>6} ms  ({}/sec)",
        fmt_n(count), ms, fmt_n(rate));
    if verbose && count > 0 {
        println!("           first={} last={}", ids[0], ids[count - 1]);
    }
    ids
}

fn insert_lives_in_edges(
    db: &mut LayeredGraphDatabase,
    people: &[crate::core::node::NodeId],
    cities: &[crate::core::node::NodeId],
    _verbose: bool,
) {
    if cities.is_empty() { return; }
    let start = Instant::now();

    for (i, &person) in people.iter().enumerate() {
        let city = cities[i % cities.len()];
        let mut props = HashMap::new();
        props.insert("since".into(), Value::Integer(2000 + (i % 25) as i64));
        db.insert_edge(person, city, "LIVES_IN", 1.0, props).ok();
    }

    let count = people.len();
    let ms = start.elapsed().as_millis();
    let rate = if ms > 0 { (count as u128 * 1000 / ms) as usize } else { count };
    println!("  LIVES_IN {:>8} edges  {:>6} ms  ({}/sec)",
        fmt_n(count), ms, fmt_n(rate));
}

fn insert_knows_edges(
    db: &mut LayeredGraphDatabase,
    people: &[crate::core::node::NodeId],
    _count: usize,
    _verbose: bool,
) {
    if people.len() < 2 { return; }
    let start = Instant::now();
    let mut inserted = 0;

    // Each person knows the next 2 people (simple ring + skip-1 pattern).
    for i in 0..people.len() {
        for offset in [1_usize, 3] {
            let j = (i + offset) % people.len();
            let mut props = HashMap::new();
            props.insert("strength".into(), Value::Float((i % 10) as f64 / 10.0));
            db.insert_edge(people[i], people[j], "KNOWS", 1.0, props).ok();
            inserted += 1;
        }
    }

    let ms = start.elapsed().as_millis();
    let rate = if ms > 0 { (inserted as u128 * 1000 / ms) as usize } else { inserted };
    println!("  KNOWS    {:>8} edges  {:>6} ms  ({}/sec)",
        fmt_n(inserted), ms, fmt_n(rate));
}

// ── Benchmark runner ──────────────────────────────────────────────────────────

#[allow(dead_code)]
struct BenchResult {
    result_count: usize,
    times:        Vec<Duration>,
}

fn run_benchmark(
    db:    &mut LayeredGraphDatabase,
    desc:  &str,
    query: &str,
    runs:  usize,
) {
    let mut times = Vec::with_capacity(runs);
    let mut count = 0;

    for _ in 0..runs {
        let t = Instant::now();
        let result = db.execute_query(&SimpleQueryLanguage, query).unwrap_or(
            crate::query::result::QueryResult::Empty
        );
        times.push(t.elapsed());
        count = result_count(&result);
    }

    let min_ms  = times.iter().map(|d| d.as_micros()).min().unwrap_or(0);
    let avg_ms  = times.iter().map(|d| d.as_micros()).sum::<u128>() / runs as u128;
    let max_ms  = times.iter().map(|d| d.as_micros()).max().unwrap_or(0);

    println!("  {:<58}", truncate(desc, 58));
    println!("    query: {}", truncate(query, 60));
    println!("    min {:>6} µs  avg {:>6} µs  max {:>6} µs  | {} result(s)",
        min_ms, avg_ms, max_ms, fmt_n(count));
    println!();
}

fn result_count(r: &crate::query::result::QueryResult) -> usize {
    use crate::query::result::QueryResult::*;
    match r {
        Nodes(v)      => v.len(),
        Edges(v)      => v.len(),
        Traversal(v)  => v.len(),
        Count(n)      => *n,
        Path { nodes, .. } => nodes.len(),
        SingleNode(Some(_)) | SingleEdge(Some(_)) => 1,
        _             => 0,
    }
}

// ── Full-scan vs index comparison ─────────────────────────────────────────────

fn compare_scan_vs_index(db: &mut LayeredGraphDatabase, runs: usize) {
    // We can force a "full scan" by using a property NOT in the index — but
    // since we index all fields, we'll compare: label query (indexed) vs
    // a property query that will use the property index vs COUNT NODES (O(1)).
    let n = db.node_count();

    let cases: &[(&str, &str)] = &[
        ("COUNT NODES (O(1))",
         "COUNT NODES"),
        ("Label index scan: Cities  O(label_count)",
         "MATCH NODE WHERE label = \"City\""),
        ("Property index: population > 5 000 000  O(log N + k)",
         "MATCH NODE WHERE props.population > 5000000"),
        ("Full match (no filter, all nodes)  O(N)",
         "MATCH NODE"),
    ];

    println!("  Total nodes in database: {}\n", fmt_n(n));
    println!("  {:<52}  {:>8}  {:>8}", "Strategy", "avg µs", "results");
    println!("  {}", "-".repeat(72));

    for (label, query) in cases {
        let mut total_us = 0u128;
        let mut count = 0;
        for _ in 0..runs {
            let t = Instant::now();
            let r = db.execute_query(&SimpleQueryLanguage, query)
                .unwrap_or(crate::query::result::QueryResult::Empty);
            total_us += t.elapsed().as_micros();
            count = result_count(&r);
        }
        let avg_us = total_us / runs as u128;
        println!("  {:<52}  {:>8}  {:>8}", truncate(label, 52), avg_us, fmt_n(count));
    }
    println!();
}

// ── Cache effectiveness ───────────────────────────────────────────────────────

fn cache_effectiveness_report(db: &mut LayeredGraphDatabase) {
    db.reset_metrics();

    // Cold pass: load 1000 nodes by ID (cache misses)
    let cold_start = Instant::now();
    for i in 0..1000_u64 {
        db.get_node(crate::core::node::NodeId(i)).ok();
    }
    let cold_ms = cold_start.elapsed().as_millis();
    let cold_hits   = db.metrics().cache_node_hits;
    let cold_misses = db.metrics().cache_node_misses;

    // Warm pass: same 1000 nodes (cache hits)
    let warm_start = Instant::now();
    for i in 0..1000_u64 {
        db.get_node(crate::core::node::NodeId(i)).ok();
    }
    let warm_ms = warm_start.elapsed().as_millis();
    let warm_hits = db.metrics().cache_node_hits - cold_hits;

    let speedup = if warm_ms > 0 { cold_ms / warm_ms } else { cold_ms };

    println!("  1 000 random node lookups:");
    println!("    Cold (cache misses): {} ms  ({} hits, {} misses)",
        cold_ms, cold_hits, cold_misses);
    println!("    Warm (cache hits):   {} ms  ({} hits)", warm_ms, warm_hits);
    if speedup > 1 {
        println!("    Cache speedup: {}×", speedup);
    }
    println!("    Node cache hit rate: {:.1}%",
        db.metrics().cache_node_hit_rate() * 100.0);
    println!();
}

// ── Schema report ─────────────────────────────────────────────────────────────

fn schema_report(db: &LayeredGraphDatabase) {
    println!("  Nodes: {}  |  Edges: {}",
        fmt_n(db.node_count()), fmt_n(db.edge_count()));
    println!();
    println!("  Labels (top 10 by count):");
    for (label, count) in db.label_stats().iter().take(10) {
        println!("    {:30} {:>10} nodes", label, fmt_n(*count));
    }
    println!();
    let indexed = db.indexed_fields();
    println!("  Indexed property fields ({}):", indexed.len());
    for field in &indexed {
        println!("    {field}");
    }
    println!();
    println!("  WAL size: {} KB", db.wal_size_bytes() / 1024);
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn fmt_n(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}…", &s[..max.saturating_sub(1)]) }
}

fn section(title: &str) -> String {
    let line = "-".repeat(62);
    format!("{line}\n  {title}\n{line}")
}

fn print_banner() {
    println!("{}", "=".repeat(62));
    println!("  AdGraphDb Scale Runbook");
    println!("  Tests query performance, indexing, and cache effectiveness");
    println!("{}", "=".repeat(62));
}
