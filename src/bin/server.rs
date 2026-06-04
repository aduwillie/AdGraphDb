// server binary — starts an AdGraphDb server on a TCP port.
//
// Usage:
//   cargo run --bin server -- [OPTIONS]
//
// Options:
//   --db <path>        Storage file path (default: graph.json)
//   --format json|bin  Storage format   (default: json)
//   --port <n>         TCP port         (default: 7474)
//   --cache <n>        LRU cache size   (default: 512 nodes, 1024 edges)
//   --help             Print this help
//
// Examples:
//   cargo run --bin server
//   cargo run --bin server -- --db mydb.bin --format bin --port 9000
//   cargo run --bin server -- --db cities.json --port 7474

use std::collections::HashMap;
use std::process;

use ad_graph_db::{
    adapters::{
        cache::{lru::LruCache, no_cache::NoCache},
        engine::adjacency_list::AdjacencyListEngine,
        storage::{binary_file::BinaryFileStorage, json_file::JsonFileStorage},
    },
    concurrent::SharedDatabase,
    database::layered::LayeredGraphDatabase,
    ports::{cache::CachePort, storage::StoragePort},
    server::GraphServer,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let opts = parse_args(&args);

    let db_path   = opts.get("db").map(|s| s.as_str()).unwrap_or("graph.json");
    let format    = opts.get("format").map(|s| s.as_str()).unwrap_or("json");
    let port      = opts.get("port").map(|s| s.as_str()).unwrap_or("7474");
    let cache_n   = opts.get("cache").and_then(|s| s.parse::<usize>().ok()).unwrap_or(512);
    let addr      = format!("0.0.0.0:{port}");

    println!("AdGraphDb Server");
    println!("  DB file : {db_path}  (format: {format})");
    println!("  Listen  : {addr}");
    println!("  Cache   : {cache_n} nodes / {} edges", cache_n * 2);
    println!();

    // ── Open storage ─────────────────────────────────────────────────────────
    let storage: Box<dyn StoragePort> = match format {
        "bin" | "binary" => {
            Box::new(BinaryFileStorage::open(db_path).unwrap_or_else(|e| {
                eprintln!("Failed to open binary DB '{db_path}': {e}");
                process::exit(1);
            }))
        }
        _ => {
            Box::new(JsonFileStorage::open(db_path).unwrap_or_else(|e| {
                eprintln!("Failed to open JSON DB '{db_path}': {e}");
                process::exit(1);
            }))
        }
    };

    // ── Open cache ────────────────────────────────────────────────────────────
    let cache: Box<dyn CachePort> = if cache_n == 0 {
        Box::new(NoCache)
    } else {
        Box::new(LruCache::new(cache_n, cache_n * 2))
    };

    // ── Assemble database ─────────────────────────────────────────────────────
    let db = LayeredGraphDatabase::open(
        storage,
        cache,
        Box::new(AdjacencyListEngine::new()),
    )
    .unwrap_or_else(|e| {
        eprintln!("Failed to open database: {e}");
        process::exit(1);
    });

    println!("Database opened: {} node(s), {} edge(s)",
        db.node_count(), db.edge_count());

    // ── Start server ──────────────────────────────────────────────────────────
    let shared = SharedDatabase::new(db);
    let server = GraphServer::new(shared, addr);

    if let Err(e) = server.start() {
        eprintln!("Server error: {e}");
        process::exit(1);
    }
}

fn parse_args(args: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].trim_start_matches('-').to_string();
        if i + 1 < args.len() && !args[i + 1].starts_with('-') {
            map.insert(key, args[i + 1].clone());
            i += 2;
        } else {
            map.insert(key, "true".into());
            i += 1;
        }
    }
    map
}

fn print_help() {
    println!("AdGraphDb Server");
    println!();
    println!("USAGE:");
    println!("  cargo run --bin server -- [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  --db <path>          Storage file  (default: graph.json)");
    println!("  --format json|bin    Format        (default: json)");
    println!("  --port <n>           TCP port      (default: 7474)");
    println!("  --cache <n>          LRU node cap  (default: 512; 0 = disabled)");
    println!("  --help               Print this message");
    println!();
    println!("PROTOCOL (line-based text over TCP):");
    println!("  Send:    [simple:|cypher:]<query>  then newline");
    println!("  Receive: OK or ERR, then result, then ---END---");
    println!();
    println!("EXAMPLES:");
    println!("  cargo run --bin server");
    println!("  cargo run --bin server -- --db cities.bin --format bin --port 9000");
}
