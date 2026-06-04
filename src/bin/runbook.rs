// runbook binary — standalone scale runbook launcher.
//
// Identical to `cargo run --bin cli -- runbook` but without the REPL wrapper.
//
// Usage:
//   cargo run --bin runbook
//   cargo run --bin runbook -- --cities 5000 --people 20000
//   cargo run --bin runbook -- --help

use std::collections::HashMap;

use ad_graph_db::runbook::{run, RunbookConfig};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let opts = parse_kv_args(&args);

    let serve_addr = opts.get("serve").map(|v| {
        if v == "true" { "127.0.0.1:7474".to_string() } else { v.clone() }
    });

    // Start from defaults so new RunbookConfig fields don't break this binary.
    let cfg = RunbookConfig {
        city_count:   opts.get("cities").and_then(|s| s.parse().ok()).unwrap_or(10_000),
        person_count: opts.get("people").and_then(|s| s.parse().ok()).unwrap_or(50_000),
        bench_runs:   opts.get("runs").and_then(|s| s.parse().ok()).unwrap_or(3),
        concurrency:  opts.get("concurrency").and_then(|s| s.parse().ok()).unwrap_or(8),
        load_queries_per_thread:
                      opts.get("load-queries").and_then(|s| s.parse().ok()).unwrap_or(500),
        verbose:      !opts.contains_key("quiet"),
        db_path:      opts.get("db").cloned().unwrap_or_else(|| "runbook_temp.json".into()),
        serve_addr,
    };

    run(cfg);
}

fn print_help() {
    println!("AdGraphDb Scale Runbook");
    println!();
    println!("USAGE:");
    println!("  cargo run --bin runbook [OPTIONS]");
    println!("  cargo run --bin cli -- runbook [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  --cities <n>        City nodes        (default: 10 000)");
    println!("  --people <n>        Person nodes      (default: 50 000)");
    println!("  --runs <n>          Benchmark reps    (default: 3)");
    println!("  --concurrency <n>   Load-test threads (default: 8)");
    println!("  --load-queries <n>  Queries/thread    (default: 500)");
    println!("  --db <path>         Temp DB path");
    println!("  --serve [addr]      Serve loaded data after benchmarks");
    println!("  --quiet             Less verbose");
    println!("  --help              This message");
}

fn parse_kv_args(args: &[String]) -> HashMap<String, String> {
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
