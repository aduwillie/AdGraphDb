// AdGraphDb CLI — interactive query REPL and runbook launcher.
//
// ── Subcommands ───────────────────────────────────────────────────────────────
//
//   (default)             Interactive REPL — remote or embedded
//   runbook               Scale runbook: insert synthetic data + benchmark
//
// ── REPL modes ────────────────────────────────────────────────────────────────
//
//   Remote   cargo run --bin cli                          → connects to 127.0.0.1:7474
//            cargo run --bin cli -- --server host:9000    → custom address
//
//   Embedded cargo run --bin cli -- --db graph.json       → opens file directly
//            cargo run --bin cli -- --db db.bin --format bin
//
// ── Runbook ───────────────────────────────────────────────────────────────────
//
//   cargo run --bin cli -- runbook
//   cargo run --bin cli -- runbook --cities 5000 --people 20000
//   cargo run --bin cli -- runbook --help

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::process;
use std::time::Instant;

use ad_graph_db::{
    adapters::{
        cache::{lru::LruCache, no_cache::NoCache},
        engine::adjacency_list::AdjacencyListEngine,
        storage::{binary_file::BinaryFileStorage, json_file::JsonFileStorage},
    },
    database::layered::LayeredGraphDatabase,
    ports::{cache::CachePort, storage::StoragePort},
    query::{
        languages::{cypher_lite::CypherLiteLanguage, simple::SimpleQueryLanguage},
        result::QueryResult,
    },
    runbook::{run as run_runbook_lib, RunbookConfig},
};

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("runbook") => {
            run_runbook_subcommand(&args[1..]);
        }
        Some("--help") | Some("-h") => {
            print_main_help();
        }
        _ => {
            run_repl_subcommand(&args);
        }
    }
}

// ── Runbook subcommand ────────────────────────────────────────────────────────

fn run_runbook_subcommand(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_runbook_help();
        return;
    }

    let opts = parse_kv_args(args);

    // `--serve` with no value defaults to 127.0.0.1:7474; `--serve host:port` overrides.
    let serve_addr = opts.get("serve").map(|v| {
        if v == "true" { "127.0.0.1:7474".to_string() } else { v.clone() }
    });

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

    run_runbook_lib(cfg);
}

fn print_runbook_help() {
    println!("AdGraphDb Scale Runbook");
    println!();
    println!("USAGE:");
    println!("  cargo run --bin cli -- runbook [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  --cities <n>        City nodes to insert          (default: 10 000)");
    println!("  --people <n>        Person nodes                  (default: 50 000)");
    println!("  --runs <n>          Benchmark repetitions/query   (default: 3)");
    println!("  --concurrency <n>   Threads for the load test     (default: 8)");
    println!("  --load-queries <n>  Queries per load-test thread  (default: 500)");
    println!("  --db <path>         Temp DB file path");
    println!("  --serve [addr]      After benchmarks, start the server on the loaded");
    println!("                      data (default addr 127.0.0.1:7474). Keeps the file.");
    println!("  --quiet             Suppress per-node progress lines");
    println!("  --help              This message");
    println!();
    println!("PHASES:");
    println!("  1. Data ingestion      — City, Person, LIVES_IN, KNOWS data");
    println!("  2. Query benchmarks    — All query types with timing");
    println!("  3. Scan vs index       — Full scan vs label/property index comparison");
    println!("  4. Cache effectiveness — Cold vs warm lookup timing");
    println!("  5. Metrics             — Full DatabaseMetrics report");
    println!("  6. Schema              — Labels, indexed fields, WAL size");
    println!("  7. Concurrent load     — Multi-threaded throughput (server model)");
    println!("  8. Serve (--serve)     — Start the TCP server on the loaded dataset");
    println!();
    println!("EXAMPLES:");
    println!("  cargo run --bin cli -- runbook");
    println!("  cargo run --bin cli -- runbook --cities 5000 --concurrency 16");
    println!("  cargo run --bin cli -- runbook --serve         # load data, then serve it");
    println!("  cargo run --bin cli -- runbook --serve 0.0.0.0:9000");
}

// ── REPL subcommand ───────────────────────────────────────────────────────────

fn run_repl_subcommand(args: &[String]) {
    let opts = parse_kv_args(args);

    if let Some(db_path) = opts.get("db") {
        run_embedded(db_path, &opts);
    } else {
        let addr = opts.get("server").cloned().unwrap_or_else(|| "127.0.0.1:7474".into());
        run_remote(&addr);
    }
}

// ── REPL state ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Lang { Simple, Cypher }
impl Lang {
    fn name(self) -> &'static str { match self { Lang::Simple => "simple", Lang::Cypher => "cypher" } }
}

struct ReplState {
    lang:         Lang,
    show_timing:  bool,
    result_limit: usize,        // 0 = unlimited
    history:      Vec<String>,
}

impl ReplState {
    fn new() -> Self {
        Self { lang: Lang::Simple, show_timing: true, result_limit: 50, history: Vec::new() }
    }

    fn record(&mut self, line: &str) {
        if !line.is_empty() && self.history.last().map(|s: &String| s != line).unwrap_or(true) {
            self.history.push(line.to_string());
            if self.history.len() > 100 { self.history.remove(0); }
        }
    }

    fn prompt(&self) -> String {
        format!("db({})> ", self.lang.name())
    }
}

// ── Embedded REPL ─────────────────────────────────────────────────────────────

fn run_embedded(db_path: &str, opts: &HashMap<String, String>) {
    let format  = opts.get("format").map(|s| s.as_str()).unwrap_or("json");
    let cache_n = opts.get("cache").and_then(|s| s.parse::<usize>().ok()).unwrap_or(512);

    let storage: Box<dyn StoragePort> = match format {
        "bin" | "binary" => Box::new(BinaryFileStorage::open(db_path).unwrap_or_else(|e| {
            eprintln!("Cannot open '{db_path}': {e}"); process::exit(1);
        })),
        _ => Box::new(JsonFileStorage::open(db_path).unwrap_or_else(|e| {
            eprintln!("Cannot open '{db_path}': {e}"); process::exit(1);
        })),
    };

    let cache: Box<dyn CachePort> = if cache_n == 0 {
        Box::new(NoCache)
    } else {
        Box::new(LruCache::new(cache_n, cache_n * 2))
    };

    let mut db = LayeredGraphDatabase::open(
        storage, cache, Box::new(AdjacencyListEngine::new()),
    ).unwrap_or_else(|e| { eprintln!("Cannot open database: {e}"); process::exit(1); });

    println!("AdGraphDb CLI  [embedded: {db_path}]");
    println!("  {} node(s)  {} edge(s)  |  :help for commands",
        db.node_count(), db.edge_count());
    if db.node_count() == 0 {
        println!("  Empty database — type :seed to load sample data and start querying.");
    }
    println!();

    let mut state = ReplState::new();
    let stdin = io::stdin();

    loop {
        print!("{}", state.prompt());
        io::stdout().flush().ok();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim().to_string();
        if line.is_empty() { continue; }

        state.record(&line);

        if handle_quit(&line) { break; }

        if handle_meta_embedded(&line, &mut state, &mut db) { continue; }

        // Language prefix / default
        let (lang, query) = split_lang_prefix(&line, state.lang);

        let start = Instant::now();
        match lang {
            Lang::Simple => run_query_embedded(&mut db, &SimpleQueryLanguage, query, &state),
            Lang::Cypher => run_query_embedded(&mut db, &CypherLiteLanguage,  query, &state),
        }
        let elapsed_ms = start.elapsed().as_millis();
        if state.show_timing {
            println!("  ({}ms)", elapsed_ms);
        }
    }

    println!("Goodbye!");
}

fn run_query_embedded(
    db:    &mut LayeredGraphDatabase,
    lang:  &dyn ad_graph_db::query::port::QueryLanguagePort,
    query: &str,
    state: &ReplState,
) {
    match db.execute_query(lang, query) {
        Ok(result)  => print_result(&result, state.result_limit),
        Err(e)      => println!("Error: {e}"),
    }
}

// ── Sample data seeder ────────────────────────────────────────────────────────
//
// Inserts a small European-cities rail graph so a new user can run queries
// immediately.  Idempotent guard: refuses if the database already has nodes.

fn seed_sample_graph(db: &mut LayeredGraphDatabase) {
    use ad_graph_db::core::value::Value;

    if db.node_count() > 0 {
        println!("Database already has {} node(s) — :seed only runs on an empty database.",
            db.node_count());
        println!("Tip: open a fresh file with --db <new-path> to seed sample data.");
        return;
    }

    // (name, population, country, coastal)
    let cities = [
        ("London",    9_000_000_i64, "UK",          false),
        ("Paris",     2_100_000,     "France",      false),
        ("Berlin",    3_700_000,     "Germany",     false),
        ("Amsterdam", 870_000,       "Netherlands", true),
        ("Brussels",  1_200_000,     "Belgium",     false),
    ];

    let mut ids = Vec::new();
    for (name, pop, country, coastal) in cities {
        let mut props = HashMap::new();
        props.insert("name".to_string(),       Value::Text(name.to_string()));
        props.insert("population".to_string(), Value::Integer(pop));
        props.insert("country".to_string(),    Value::Text(country.to_string()));
        props.insert("coastal".to_string(),    Value::Boolean(coastal));
        match db.insert_node("City", props) {
            Ok(id) => ids.push(id),
            Err(e) => { println!("Seed failed: {e}"); return; }
        }
    }

    // Rail links (source_idx, target_idx, km)
    let rails = [
        (0, 1, 457.0), // London → Paris
        (0, 4, 370.0), // London → Brussels
        (1, 4, 265.0), // Paris → Brussels
        (1, 2, 1054.0),// Paris → Berlin
        (4, 3, 210.0), // Brussels → Amsterdam
        (3, 2, 660.0), // Amsterdam → Berlin
    ];

    let mut edge_count = 0;
    for (s, t, km) in rails {
        if let (Some(&src), Some(&tgt)) = (ids.get(s), ids.get(t)) {
            if db.insert_edge(src, tgt, "RAIL", km, HashMap::new()).is_ok() {
                edge_count += 1;
            }
        }
    }

    println!("Seeded {} cities and {} rail links.", ids.len(), edge_count);
    println!("Try:");
    println!("  MATCH NODE WHERE label = \"City\"");
    println!("  MATCH NODE WHERE props.population > 1000000");
    println!("  TRAVERSE BFS FROM {}", ids[0]);
    println!("  PATH FROM {} TO {}", ids[0], ids[2]);
}

// ── Meta-command handler (embedded) ──────────────────────────────────────────
//
// Returns true if the line was a meta-command and was consumed.

fn handle_meta_embedded(
    line:  &str,
    state: &mut ReplState,
    db:    &mut LayeredGraphDatabase,
) -> bool {
    let lower = line.to_lowercase();
    let lower = lower.trim();

    // Language switching
    if lower == ":use simple" || lower == ":lang simple" {
        state.lang = Lang::Simple; println!("Language: SimpleQuery"); return true;
    }
    if lower == ":use cypher" || lower == ":lang cypher" {
        state.lang = Lang::Cypher; println!("Language: CypherLite"); return true;
    }
    if lower == ":lang" {
        println!("Language: {}", state.lang.name()); return true;
    }

    // Quick stats
    if lower == ":nodes" {
        println!("Nodes: {}", db.node_count()); return true;
    }
    if lower == ":edges" {
        println!("Edges: {}", db.edge_count()); return true;
    }

    // Label / schema
    if lower == ":labels" {
        let stats = db.label_stats();
        if stats.is_empty() {
            println!("(no nodes)");
        } else {
            println!("  {:<30}  {:>10}", "Label", "Count");
            println!("  {}", "-".repeat(43));
            for (label, count) in &stats {
                println!("  {:<30}  {:>10}", label, count);
            }
        }
        return true;
    }

    if lower == ":schema" {
        println!("  Nodes: {}  |  Edges: {}", db.node_count(), db.edge_count());
        println!();
        let labels = db.label_stats();
        println!("  Labels ({}):", labels.len());
        for (label, count) in labels.iter().take(20) {
            println!("    {:<30} {:>8} nodes", label, count);
        }
        println!();
        let indexed = db.indexed_fields();
        println!("  Indexed property fields ({}):", indexed.len());
        for f in &indexed { println!("    {f}"); }
        println!();
        println!("  WAL size: {} bytes", db.wal_size_bytes());
        return true;
    }

    // Metrics
    if lower == ":stats" || lower == ":metrics" {
        println!("{}", db.metrics()); return true;
    }

    // Compaction
    if lower == ":compact" {
        match db.compact() {
            Ok(())  => println!("WAL compacted.  Size now: {} bytes", db.wal_size_bytes()),
            Err(e)  => println!("Compact failed: {e}"),
        }
        return true;
    }

    // Seed sample data — gives a brand-new user something to query instantly.
    if lower == ":seed" {
        seed_sample_graph(db);
        return true;
    }

    // Timing toggle
    if lower == ":time" {
        println!("Timing: {}", if state.show_timing { "on" } else { "off" });
        return true;
    }
    if lower == ":time on"  { state.show_timing = true;  println!("Timing: on");  return true; }
    if lower == ":time off" { state.show_timing = false; println!("Timing: off"); return true; }

    // Result limit
    if lower == ":limit" {
        println!("Result limit: {}", if state.result_limit == 0 { "unlimited".into() } else { state.result_limit.to_string() });
        return true;
    }
    if let Some(n_str) = lower.strip_prefix(":limit ") {
        match n_str.trim().parse::<usize>() {
            Ok(n)  => { state.result_limit = n; println!("Result limit: {}", if n == 0 { "unlimited".into() } else { n.to_string() }); }
            Err(_) => println!("Usage: :limit <number>  (0 = unlimited)"),
        }
        return true;
    }

    // Explain
    if let Some(query) = line.strip_prefix(":explain ").or_else(|| line.strip_prefix(":explain\t")) {
        let query = query.trim();
        let lang_impl: &dyn ad_graph_db::query::port::QueryLanguagePort = match state.lang {
            Lang::Simple => &SimpleQueryLanguage,
            Lang::Cypher => &CypherLiteLanguage,
        };
        match db.execute_query_with_explain(lang_impl, query) {
            Ok((_, plan)) => println!("Plan: {plan}"),
            Err(e)        => println!("Error: {e}"),
        }
        return true;
    }

    // History
    if lower == ":history" {
        let hist = &state.history;
        let start = hist.len().saturating_sub(20);
        for (i, cmd) in hist[start..].iter().enumerate() {
            println!("  {:3}  {cmd}", start + i + 1);
        }
        return true;
    }

    // Run from file
    if let Some(path) = line.strip_prefix(":run ").or_else(|| line.strip_prefix(":run\t")) {
        run_file_embedded(path.trim(), state, db);
        return true;
    }

    // Clear screen
    if lower == ":clear" || lower == ":cls" {
        print!("\x1B[2J\x1B[H"); io::stdout().flush().ok(); return true;
    }

    // Help
    if lower == ":help" {
        print_help_overview(); return true;
    }
    if let Some(topic) = line.strip_prefix(":help ").or_else(|| line.strip_prefix(":help\t")) {
        print_help_topic(topic.trim()); return true;
    }

    false // not a meta-command
}

fn run_file_embedded(
    path:  &str,
    state: &mut ReplState,
    db:    &mut LayeredGraphDatabase,
) {
    let content = match std::fs::read_to_string(path) {
        Ok(c)  => c,
        Err(e) => { println!("Cannot read '{path}': {e}"); return; }
    };

    let mut n = 0;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        println!("{}{line}", state.prompt());
        state.record(line);
        if handle_quit(line) { break; }
        if handle_meta_embedded(line, state, db) { n += 1; continue; }
        let (lang, query) = split_lang_prefix(line, state.lang);
        match lang {
            Lang::Simple => run_query_embedded(db, &SimpleQueryLanguage, query, state),
            Lang::Cypher => run_query_embedded(db, &CypherLiteLanguage,  query, state),
        }
        n += 1;
    }
    println!("  ({n} commands executed from '{path}')");
}

// ── Remote REPL ───────────────────────────────────────────────────────────────

fn run_remote(addr: &str) {
    let stream = TcpStream::connect(addr).unwrap_or_else(|e| {
        eprintln!("Cannot connect to {addr}: {e}");
        eprintln!("  Start the server with: cargo run --bin server");
        process::exit(1);
    });

    println!("AdGraphDb CLI  [remote → {addr}]");
    println!("  :help for commands  |  :quit to exit");
    println!();

    let reader_stream = stream.try_clone().expect("clone stream");
    let reader        = BufReader::new(reader_stream);
    let mut writer    = BufWriter::new(stream);
    let mut lines_iter = reader.lines();

    read_until_end(&mut lines_iter); // consume server banner

    let stdin = io::stdin();
    let mut state = ReplState::new();

    loop {
        print!("{}", state.prompt());
        io::stdout().flush().ok();

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let input = input.trim().to_string();
        if input.is_empty() { continue; }

        state.record(&input);

        // Local-only meta-commands (don't need a round-trip)
        if handle_meta_remote_local(&input, &mut state) { continue; }
        if handle_quit(&input) {
            send_and_read(&mut writer, &mut lines_iter, ":quit");
            break;
        }

        // Build the wire query (prefix with language)
        let (lang, query) = split_lang_prefix(&input, state.lang);
        let wire = match lang {
            Lang::Simple => format!("simple:{query}"),
            Lang::Cypher => format!("cypher:{query}"),
        };

        let start = Instant::now();
        let response = send_and_read(&mut writer, &mut lines_iter, &wire);
        let elapsed_ms = start.elapsed().as_millis();

        // Strip OK/ERR status line and print
        let body = response.trim_start_matches("OK\n").trim_start_matches("ERR\n");
        println!("{body}");
        if state.show_timing { println!("  ({}ms round-trip)", elapsed_ms); }
    }

    println!("Goodbye!");
}

fn handle_meta_remote_local(line: &str, state: &mut ReplState) -> bool {
    let lower = line.to_lowercase();
    let lower = lower.trim();
    match lower {
        ":use simple" | ":lang simple" => { state.lang = Lang::Simple; println!("Language: SimpleQuery"); true }
        ":use cypher" | ":lang cypher" => { state.lang = Lang::Cypher; println!("Language: CypherLite"); true }
        ":lang" => { println!("Language: {}", state.lang.name()); true }
        ":time on"  => { state.show_timing = true;  println!("Timing: on");  true }
        ":time off" => { state.show_timing = false; println!("Timing: off"); true }
        ":time"     => { println!("Timing: {}", if state.show_timing { "on" } else { "off" }); true }
        ":history"  => {
            let hist = &state.history;
            let start = hist.len().saturating_sub(20);
            for (i, cmd) in hist[start..].iter().enumerate() { println!("  {:3}  {cmd}", start+i+1); }
            true
        }
        ":clear" | ":cls" => { print!("\x1B[2J\x1B[H"); io::stdout().flush().ok(); true }
        ":help"     => { print_help_overview(); true }
        _ if lower.starts_with(":help ") => {
            print_help_topic(&line[6..]); true
        }
        _ if lower.starts_with(":limit") => {
            let n_str = lower.trim_start_matches(":limit").trim();
            if n_str.is_empty() {
                println!("Result limit: {}", if state.result_limit == 0 { "unlimited".into() } else { state.result_limit.to_string() });
            } else if let Ok(n) = n_str.parse::<usize>() {
                state.result_limit = n;
                println!("Result limit: {}", if n == 0 { "unlimited".into() } else { n.to_string() });
            }
            true
        }
        _ => false,
    }
}

fn send_and_read(
    writer: &mut BufWriter<TcpStream>,
    lines:  &mut impl Iterator<Item = std::io::Result<String>>,
    query:  &str,
) -> String {
    let _ = writeln!(writer, "{query}");
    let _ = writer.flush();
    read_until_end(lines)
}

fn read_until_end(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> String {
    let mut parts = Vec::new();
    for line in lines.by_ref() {
        match line {
            Ok(l) if l.trim() == "---END---" => break,
            Ok(l)  => parts.push(l),
            Err(_) => break,
        }
    }
    parts.join("\n")
}

// ── Result display ────────────────────────────────────────────────────────────

fn print_result(result: &QueryResult, limit: usize) {
    match result {
        QueryResult::Nodes(nodes) => {
            let total = nodes.len();
            let show  = if limit == 0 { total } else { total.min(limit) };
            println!("Nodes ({total}):");
            for node in &nodes[..show] {
                println!("  {node}");
            }
            if show < total {
                println!("  … {} more (use :limit 0 to see all)", total - show);
            }
        }
        QueryResult::Edges(edges) => {
            let total = edges.len();
            let show  = if limit == 0 { total } else { total.min(limit) };
            println!("Edges ({total}):");
            for edge in &edges[..show] {
                println!("  {edge}");
            }
            if show < total {
                println!("  … {} more (use :limit 0 to see all)", total - show);
            }
        }
        QueryResult::Traversal(ids) => {
            let joined: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
            println!("Traversal [{}]: {}", ids.len(), joined.join(" → "));
        }
        QueryResult::Path { nodes, total_weight } => {
            let joined: Vec<String> = nodes.iter().map(|id| id.to_string()).collect();
            println!("Path (weight {total_weight:.2}): {}", joined.join(" → "));
        }
        QueryResult::SingleNode(Some(node)) => println!("{node}"),
        QueryResult::SingleNode(None)       => println!("(not found)"),
        QueryResult::SingleEdge(Some(edge)) => println!("{edge}"),
        QueryResult::SingleEdge(None)       => println!("(not found)"),
        QueryResult::Count(n)               => println!("{n}"),
        QueryResult::Empty                  => println!("(no results)"),
    }
}

// ── Help system ───────────────────────────────────────────────────────────────

fn print_help_overview() {
    println!(r#"
AdGraphDb CLI Help
==================

QUICK REFERENCE
  :help queries      SimpleQuery and CypherLite syntax
  :help meta         All meta-commands (:nodes, :stats, :explain, ...)
  :help examples     Copy-paste runnable examples
  :help bfs          BFS traversal explained
  :help dijkstra     Shortest path explained
  :help transactions Transaction API
  :help runbook      Scale runbook (cargo run --bin cli -- runbook)

LANGUAGE
  :use simple        Switch to SimpleQuery  (default)
  :use cypher        Switch to CypherLite

  Per-query prefix (override default language for one query):
    simple: MATCH NODE WHERE label = "City"
    cypher: MATCH (n:City) RETURN n

GETTING STARTED (empty database?)
  :seed              Load a sample city graph to play with   [embedded]

QUICK SHORTCUTS
  :nodes             Count nodes
  :edges             Count edges
  :labels            List all labels with counts   [embedded]
  :schema            Labels + indexed fields        [embedded]
  :stats             Show runtime metrics           [embedded]
  :compact           Compact WAL                    [embedded]

QUERY TOOLS
  :explain <query>   Show execution plan            [embedded]
  :time on|off       Toggle query timing (default: on)
  :limit <n>         Max results shown (0 = unlimited, default: 50)

HISTORY & FILES
  :history           Show last 20 commands
  :run <path>        Execute queries from a file

NAVIGATION
  :clear / :cls      Clear screen
  :quit / :exit      Exit
"#);
}

fn print_help_topic(topic: &str) {
    match topic.to_lowercase().as_str() {
        "queries" | "query" | "dsl" => print_help_queries(),
        "meta" | "commands"         => print_help_meta(),
        "examples" | "example"      => print_help_examples(),
        "bfs"                       => print_help_bfs(),
        "dijkstra" | "path" | "shortest" => print_help_dijkstra(),
        "transactions" | "txn"      => print_help_transactions(),
        "runbook" | "bench"         => print_help_runbook(),
        other => println!("Unknown help topic '{other}'.  Try :help for the overview."),
    }
}

fn print_help_queries() {
    println!(r#"
Query Language Reference
========================

── SimpleQuery ──────────────────────────────────────────────
  MATCH NODE                                  all nodes
  MATCH NODE WHERE label = "City"             by label  (O(1) index)
  MATCH NODE WHERE label = "City"
                AND props.name = "London"     label + property filter
  MATCH NODE WHERE props.population > 1000000 property range (O(log N))
  MATCH EDGE                                  all edges
  MATCH EDGE WHERE label = "RAIL"             edges by label
  MATCH EDGE WHERE weight < 500               edges by weight
  GET NODE N5                                 point lookup (O(1))
  GET EDGE E3                                 point lookup (O(1))
  TRAVERSE BFS FROM N0                        breadth-first traversal
  TRAVERSE DFS FROM N0                        depth-first traversal
  PATH FROM N0 TO N9                          Dijkstra shortest path
  COUNT NODES                                 O(1) count
  COUNT EDGES                                 O(1) count

  Operators:  =  !=  <  <=  >  >=
  Values:     "text"   42   3.14   true   false   null

── CypherLite ───────────────────────────────────────────────
  MATCH (n) RETURN n
  MATCH (n:City) RETURN n
  MATCH (n:City) WHERE n.population > 1000000 RETURN n
  MATCH (n) WHERE n.name = "London" RETURN n
  MATCH ()-[r:RAIL]->() RETURN r
  MATCH ()-[r]->() WHERE r.weight < 500 RETURN r
  TRAVERSE BFS FROM N0
  PATH FROM N0 TO N9
  COUNT NODES
  COUNT EDGES

── Execution strategies (chosen automatically by planner) ───
  GET NODE / EDGE         O(1) cache lookup
  Label filter            O(label_count)  via LabelIndex
  Property filter         O(log N + k)    via PropertyIndex
  No filter               O(N)            full scan
  Traversal               O(V+E) in RAM
  Shortest path           O((V+E) log V) in RAM
"#);
}

fn print_help_meta() {
    println!(r#"
Meta-Commands Reference
=======================

LANGUAGE
  :use simple       Switch to SimpleQuery  (default)
  :use cypher       Switch to CypherLite
  :lang             Show current language

QUICK QUERIES  [embedded only]
  :nodes            COUNT NODES
  :edges            COUNT EDGES
  :labels           All labels and their node counts (sorted by count)
  :schema           Labels, indexed property fields, WAL size

DATABASE INFO  [embedded only]
  :stats            Full DatabaseMetrics report:
                      cache hit rate, index hits, full scans, query timing
  :metrics          Same as :stats

DATA MANAGEMENT  [embedded only]
  :seed             Load a small sample city/rail graph (empty DB only)
                    Fastest way to get data to query without the runbook
  :compact          Compact WAL — removes tombstones and overwritten records
                    Run periodically after many deletes/updates

QUERY TOOLS
  :time             Show current timing setting
  :time on          Show elapsed ms after each query result  (default)
  :time off         Hide timing
  :limit            Show current result display limit
  :limit <n>        Display at most n results  (0 = unlimited, default: 50)
  :explain <query>  Show the execution plan the planner chose [embedded]
                    Does not execute the query — plan only

HISTORY & FILES
  :history          Show last 20 commands entered this session
  :run <filepath>   Run a query file line by line  (# lines = comments)

DISPLAY
  :clear / :cls     Clear the terminal screen

EXIT
  :quit / :exit     Exit the REPL
  Ctrl-D            Same as :quit

[embedded only] = requires --db flag (embedded mode).
                  Not available when connected to a remote server.
"#);
}

fn print_help_examples() {
    println!(r#"
Runnable Examples
=================
Copy any of these directly into the prompt and press Enter.

── City graph ───────────────────────────────────────────────

  MATCH NODE WHERE label = "City"
  MATCH NODE WHERE label = "City" AND props.population > 5000000
  MATCH NODE WHERE props.name = "London"
  GET NODE N0
  MATCH EDGE WHERE label = "RAIL"
  MATCH EDGE WHERE weight < 500

── Traversal ────────────────────────────────────────────────

  TRAVERSE BFS FROM N0
  TRAVERSE DFS FROM N0
  PATH FROM N0 TO N4

── Counting ─────────────────────────────────────────────────

  COUNT NODES
  COUNT EDGES

── Meta shortcuts ───────────────────────────────────────────

  :nodes
  :edges
  :labels
  :schema
  :stats
  :explain MATCH NODE WHERE label = "City"
  :time on

── Same queries in CypherLite ───────────────────────────────

  cypher: MATCH (n:City) RETURN n
  cypher: MATCH (n:City) WHERE n.population > 5000000 RETURN n
  cypher: MATCH ()-[r:RAIL]->() RETURN r
  cypher: TRAVERSE BFS FROM N0
  cypher: PATH FROM N0 TO N4

── Tip ──────────────────────────────────────────────────────

  Use :explain before any query to see which index the planner chose:
    :explain MATCH NODE WHERE label = "City"
    :explain MATCH NODE WHERE props.population > 1000000
"#);
}

fn print_help_bfs() {
    println!(r#"
Breadth-First Search (BFS)
==========================

BFS visits nodes level-by-level from a starting node.
All nodes 1 hop away are visited before nodes 2 hops away.

Analogy: ripples spreading outward from a stone dropped in water.

Usage:
  TRAVERSE BFS FROM N0

How it works:
  1. Put N0 in a queue.  Mark N0 as visited.
  2. Dequeue N0.  Find its outgoing neighbors [N1, N2].
  3. Mark and enqueue each unvisited neighbor.
  4. Repeat until queue is empty.

Cost: O(V + E)  — every reachable node and edge visited once.
      All in RAM (adjacency engine only — no disk I/O during traversal).

Good for:
  - Discovering all reachable nodes from a start point
  - Finding the shortest number of hops between nodes
  - Level-order processing (direct friends, then friends-of-friends, ...)

Example:
  db(simple)> TRAVERSE BFS FROM N0
  Traversal [4]: N0 -> N1 -> N2 -> N3

See also: :help dijkstra  (weighted shortest path)
          :help examples  (copy-paste query list)
"#);
}

fn print_help_dijkstra() {
    println!(r#"
Dijkstra Shortest Path
======================

Finds the minimum-weight path between two nodes.
Edge weight = cost (distance, time, price, ...).

Usage:
  PATH FROM N0 TO N5

How it works:
  1. Set distance[N0] = 0, all others = infinity.
  2. Min-heap: always process the node with the lowest known distance first.
  3. For each outgoing neighbor: if current_dist + edge_weight < known_dist,
     update known_dist and record the predecessor.
  4. Stop as soon as the goal node is popped from the heap.
  5. Walk predecessor pointers backward to reconstruct the path.

Cost:         O((V + E) log V)  — entirely in RAM, no disk access.
Requirement:  Edge weights must be >= 0.

Result:
  Path (weight 457.00): N0 -> N1 -> N3
                ^total weight  ^sequence of node IDs

Good for:
  - Routing / navigation
  - Supply chain (minimum cost path)
  - Network latency analysis

See also: :help bfs  (hop-count shortest path, unweighted)
"#);
}

fn print_help_transactions() {
    println!(r#"
Transactions
============

Group multiple inserts/deletes so they all succeed or all fail.

Rust API (embedded code):
  let mut txn = db.begin_transaction();
  let london = txn.stage_insert_node("City", props_london);
  let paris  = txn.stage_insert_node("City", props_paris);
  txn.stage_insert_edge(london, paris, "RAIL", 457.0, HashMap::new());

  // Apply everything atomically:
  let result = db.commit_transaction(txn)?;

  // Or discard everything:
  db.rollback_transaction(txn);

Crash safety:
  BinaryFileStorage writes BEGIN_TXN and COMMIT_TXN markers into the WAL.
  If the process crashes between them, the partial transaction is
  automatically discarded on the next open().
  JsonFileStorage has no WAL markers — not crash-safe for multi-op commits.

Guarantees:
  All-or-nothing  — rollback discards all staged operations
  Durability      — committed data survives restart
  No isolation    — concurrent readers may see partial writes (not MVCC)

Docs: docs/14_transactions.md
"#);
}

fn print_help_runbook() {
    println!(r#"
Scale Runbook
=============

Self-contained benchmark: inserts synthetic data, times every query
strategy, measures cache effectiveness, and prints a full report.
No server required — runs entirely in-process.

Run it:
  cargo run --bin cli -- runbook
  cargo run --bin cli -- runbook --cities 5000 --people 20000 --runs 5
  cargo run --bin cli -- runbook --help

Options:
  --cities <n>    City nodes to insert           (default: 10 000)
  --people <n>    Person nodes to insert         (default: 50 000)
  --runs <n>      Benchmark repetitions per query (default: 3)
  --db <path>     Temp database path (auto-deleted after run)
  --quiet         Suppress per-phase verbose lines

Data model used:
  City nodes   — name, population, country, coastal, elevation
  Person nodes — name, age, occupation, active
  LIVES_IN edges (Person -> City)
  KNOWS edges    (Person -> Person, ring topology)

Phases:
  1. Data ingestion        — insert rates per node/edge type
  2. Query benchmarks      — min/avg/max us per query type
  3. Scan vs index         — full scan vs label/property index side-by-side
  4. Cache effectiveness   — cold vs warm lookup timing
  5. Runtime metrics       — cache hit rate, index hit rate, compaction count
  6. Schema summary        — labels, indexed fields, WAL size

Temp DB file is deleted automatically when the runbook finishes.
"#);
}

// ── Shared helpers ────────────────────────────────────────────────────────────

fn split_lang_prefix<'a>(line: &'a str, default: Lang) -> (Lang, &'a str) {
    if let Some(rest) = line.strip_prefix("simple:").or_else(|| line.strip_prefix("simple: ")) {
        return (Lang::Simple, rest.trim());
    }
    if let Some(rest) = line.strip_prefix("cypher:").or_else(|| line.strip_prefix("cypher: ")) {
        return (Lang::Cypher, rest.trim());
    }
    (default, line)
}

fn handle_quit(line: &str) -> bool {
    let l = line.to_lowercase();
    l == ":quit" || l == ":exit"
}

fn print_main_help() {
    println!("AdGraphDb CLI");
    println!();
    println!("SUBCOMMANDS:");
    println!("  (default)           Interactive REPL");
    println!("  runbook             Scale benchmark runbook");
    println!();
    println!("REPL OPTIONS:");
    println!("  --db <path>         Embedded mode: open a local database file");
    println!("  --format json|bin   Storage format (default: json)");
    println!("  --cache <n>         LRU node cache size (default: 512; 0 = off)");
    println!("  --server <addr>     Remote mode: connect to server (default: 127.0.0.1:7474)");
    println!("  --help              This message");
    println!();
    println!("EXAMPLES:");
    println!("  cargo run --bin cli                          # remote REPL");
    println!("  cargo run --bin cli -- --db graph.json       # embedded REPL");
    println!("  cargo run --bin cli -- runbook               # run scale benchmark");
    println!("  cargo run --bin cli -- runbook --cities 5000 # smaller benchmark");
    println!();
    println!("Inside the REPL, type :help for the full command reference.");
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
