// cli binary — interactive query REPL for AdGraphDb.
//
// Two modes:
//
//   Embedded (--db <path>):
//     Opens the database file directly in this process.
//     No server needed.  Great for exploration and one-off queries.
//
//   Remote (--server <host:port>, the default):
//     Connects to a running `server` binary over TCP.
//     Queries are sent over the wire; results streamed back.
//
// Usage:
//   cargo run --bin cli                          # remote, localhost:7474
//   cargo run --bin cli -- --server host:9000    # remote, custom address
//   cargo run --bin cli -- --db graph.json       # embedded, JSON storage
//   cargo run --bin cli -- --db db.bin --format bin  # embedded, binary
//
// ── REPL commands ─────────────────────────────────────────────────────────────
//
//   <query>                   Run query with current language
//   simple: <query>           Run query as SimpleQuery (one-off)
//   cypher: <query>           Run query as CypherLite (one-off)
//   :use simple               Switch default language to SimpleQuery
//   :use cypher               Switch default language to CypherLite
//   :lang                     Show current language
//   :help                     Show this help
//   :quit  / :exit / Ctrl-D   Exit the REPL
//
// ── Example session ───────────────────────────────────────────────────────────
//
//   db(simple)> MATCH NODE WHERE label = "City"
//   Nodes (4):
//     (N0 :City) {name: "London", population: 9000000}
//     ...
//
//   db(simple)> :use cypher
//   Language: CypherLite
//
//   db(cypher)> MATCH (n:City) WHERE n.population > 2000000 RETURN n
//   Nodes (3):
//     ...
//
//   db(cypher)> TRAVERSE BFS FROM N0
//   Traversal [4]: N0 → N1 → N2 → N3

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::process;

use ad_graph_db::{
    adapters::{
        cache::{lru::LruCache, no_cache::NoCache},
        engine::adjacency_list::AdjacencyListEngine,
        storage::{binary_file::BinaryFileStorage, json_file::JsonFileStorage},
    },
    database::layered::LayeredGraphDatabase,
    ports::{cache::CachePort, storage::StoragePort},
    query::languages::{cypher_lite::CypherLiteLanguage, simple::SimpleQueryLanguage},
};

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let opts = parse_args(&args);

    if let Some(db_path) = opts.get("db") {
        run_embedded(db_path, &opts);
    } else {
        let addr = opts
            .get("server")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1:7474".into());
        run_remote(&addr);
    }
}

// ── Embedded mode (direct DB access) ─────────────────────────────────────────

fn run_embedded(db_path: &str, opts: &HashMap<String, String>) {
    let format  = opts.get("format").map(|s| s.as_str()).unwrap_or("json");
    let cache_n = opts.get("cache").and_then(|s| s.parse::<usize>().ok()).unwrap_or(512);

    let storage: Box<dyn StoragePort> = match format {
        "bin" | "binary" => {
            Box::new(BinaryFileStorage::open(db_path).unwrap_or_else(|e| {
                eprintln!("Cannot open '{db_path}': {e}");
                process::exit(1);
            }))
        }
        _ => {
            Box::new(JsonFileStorage::open(db_path).unwrap_or_else(|e| {
                eprintln!("Cannot open '{db_path}': {e}");
                process::exit(1);
            }))
        }
    };

    let cache: Box<dyn CachePort> = if cache_n == 0 {
        Box::new(NoCache)
    } else {
        Box::new(LruCache::new(cache_n, cache_n * 2))
    };

    let mut db = LayeredGraphDatabase::open(
        storage,
        cache,
        Box::new(AdjacencyListEngine::new()),
    )
    .unwrap_or_else(|e| {
        eprintln!("Cannot open database: {e}");
        process::exit(1);
    });

    println!("AdGraphDb CLI  (embedded mode)");
    println!("  File : {db_path}  (format: {format})");
    println!("  Graph: {} node(s), {} edge(s)", db.node_count(), db.edge_count());
    println!("  Type :help for commands, :quit to exit\n");

    let simple = SimpleQueryLanguage;
    let cypher = CypherLiteLanguage;
    let mut lang = CliLang::Simple;
    let stdin = io::stdin();

    loop {
        print!("db({})> ", lang.name());
        io::stdout().flush().ok();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF / Ctrl-D
            Ok(_) => {}
            Err(e) => { eprintln!("Read error: {e}"); break; }
        }

        let line = line.trim();
        if line.is_empty() { continue; }

        if handle_meta_command(line, &mut lang) { continue; }
        if line.eq_ignore_ascii_case(":quit") || line.eq_ignore_ascii_case(":exit") { break; }

        // Parse optional language prefix.
        let (effective_lang, query) = split_lang_prefix(line, lang);

        let result = match effective_lang {
            CliLang::Simple => db.execute_query(&simple, query),
            CliLang::Cypher => db.execute_query(&cypher, query),
        };

        match result {
            Ok(r)  => println!("{r}"),
            Err(e) => println!("Error: {e}"),
        }
    }

    println!("Goodbye!");
}

// ── Remote mode (TCP client) ──────────────────────────────────────────────────

fn run_remote(addr: &str) {
    let stream = TcpStream::connect(addr).unwrap_or_else(|e| {
        eprintln!("Cannot connect to {addr}: {e}");
        eprintln!("Is the server running?  cargo run --bin server");
        process::exit(1);
    });

    println!("AdGraphDb CLI  (remote mode → {addr})");
    println!("  Type :help for commands, :quit to exit\n");

    let reader_stream = stream.try_clone().expect("clone stream");
    let reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);
    let mut lines_iter = reader.lines();

    // Consume the welcome banner from the server.
    read_until_end(&mut lines_iter);

    let stdin = io::stdin();
    let mut lang = CliLang::Simple;

    loop {
        print!("db({})> ", lang.name());
        io::stdout().flush().ok();

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => { eprintln!("Read error: {e}"); break; }
        }

        let input = input.trim();
        if input.is_empty() { continue; }

        // Handle local meta-commands that don't need a round-trip.
        if handle_meta_command(input, &mut lang) { continue; }

        // Translate :quit → server :quit before sending.
        let send_line = if input.eq_ignore_ascii_case(":quit") || input.eq_ignore_ascii_case(":exit") {
            ":quit".to_string()
        } else {
            // Prefix the current language for the server.
            match split_lang_prefix(input, lang) {
                (CliLang::Simple, q) => format!("simple:{q}"),
                (CliLang::Cypher, q) => format!("cypher:{q}"),
            }
        };

        // Send to server.
        if writeln!(writer, "{send_line}").is_err() { break; }
        if writer.flush().is_err() { break; }

        // Read response.
        let response = read_until_end(&mut lines_iter);
        println!("{response}");

        if input.eq_ignore_ascii_case(":quit") || input.eq_ignore_ascii_case(":exit") {
            break;
        }
    }

    println!("Goodbye!");
}

/// Read lines from the server until "---END---".
/// Returns the content lines joined together (without the terminator).
fn read_until_end(
    lines: &mut impl Iterator<Item = std::io::Result<String>>,
) -> String {
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

// ── Language tracking ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum CliLang { Simple, Cypher }

impl CliLang {
    fn name(self) -> &'static str {
        match self { CliLang::Simple => "simple", CliLang::Cypher => "cypher" }
    }
}

fn split_lang_prefix(line: &str, default: CliLang) -> (CliLang, &str) {
    if let Some(rest) = line.strip_prefix("simple:").or_else(|| line.strip_prefix("simple: ")) {
        return (CliLang::Simple, rest.trim());
    }
    if let Some(rest) = line.strip_prefix("cypher:").or_else(|| line.strip_prefix("cypher: ")) {
        return (CliLang::Cypher, rest.trim());
    }
    (default, line)
}

/// Handle :use, :lang, :help meta-commands.
/// Returns true if the command was consumed; false if it should be sent as a query.
fn handle_meta_command(line: &str, lang: &mut CliLang) -> bool {
    let lower = line.to_lowercase();

    match lower.as_str() {
        ":use simple" | ":lang simple" => {
            *lang = CliLang::Simple;
            println!("Language: SimpleQuery");
            return true;
        }
        ":use cypher" | ":lang cypher" => {
            *lang = CliLang::Cypher;
            println!("Language: CypherLite");
            return true;
        }
        ":lang" => {
            println!("Current language: {}", lang.name());
            return true;
        }
        ":help" => {
            print_repl_help();
            return true;
        }
        _ => {}
    }

    false
}

// ── Help text ─────────────────────────────────────────────────────────────────

fn print_repl_help() {
    println!("AdGraphDb REPL commands:");
    println!();
    println!("  <query>                 Run with current language");
    println!("  simple: <query>         Run as SimpleQuery (one-off)");
    println!("  cypher: <query>         Run as CypherLite  (one-off)");
    println!();
    println!("  :use simple             Switch default to SimpleQuery");
    println!("  :use cypher             Switch default to CypherLite");
    println!("  :lang                   Show current language");
    println!("  :help                   This message");
    println!("  :quit  / :exit          Exit");
    println!();
    println!("SimpleQuery examples:");
    println!("  MATCH NODE");
    println!("  MATCH NODE WHERE label = \"City\"");
    println!("  MATCH NODE WHERE label = \"City\" AND props.population > 1000000");
    println!("  MATCH EDGE WHERE label = \"RAIL\"");
    println!("  MATCH EDGE WHERE weight < 500");
    println!("  GET NODE N0");
    println!("  TRAVERSE BFS FROM N0");
    println!("  PATH FROM N0 TO N3");
    println!("  COUNT NODES");
    println!();
    println!("CypherLite examples:");
    println!("  MATCH (n:City) RETURN n");
    println!("  MATCH (n:City) WHERE n.population > 1000000 RETURN n");
    println!("  MATCH ()-[r:RAIL]->() RETURN r");
    println!("  MATCH ()-[r]->() WHERE r.weight < 500 RETURN r");
    println!("  TRAVERSE BFS FROM N0");
    println!("  PATH FROM N0 TO N3");
    println!("  COUNT NODES");
}

fn print_help() {
    println!("AdGraphDb CLI");
    println!();
    println!("USAGE:");
    println!("  cargo run --bin cli -- [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  --db <path>          Open local file directly (embedded mode)");
    println!("  --format json|bin    Storage format for --db (default: json)");
    println!("  --cache <n>          LRU node cache size (default: 512; 0 = off)");
    println!("  --server <host:port> Connect to remote server (default: 127.0.0.1:7474)");
    println!("  --help               This message");
    println!();
    println!("MODES:");
    println!("  Embedded : cargo run --bin cli -- --db graph.json");
    println!("  Remote   : cargo run --bin cli -- --server localhost:7474");
    println!("  Remote   : cargo run --bin cli   (connects to 127.0.0.1:7474 by default)");
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
