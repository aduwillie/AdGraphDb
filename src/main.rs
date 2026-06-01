// main.rs — end-to-end demonstration of AdGraphDb.
//
// This builds a small city-distance graph, exercises all three algorithm
// implementations, demonstrates persistence (data survives a simulated
// restart), and shows how to swap the storage adapter without any changes
// to the rest of the code.

use std::collections::HashMap;

use ad_graph_db::{
    adapters::{
        cache::lru::LruCache,
        engine::adjacency_list::AdjacencyListEngine,
        storage::{binary_file::BinaryFileStorage, json_file::JsonFileStorage},
    },
    algorithms::{bfs::BreadthFirstSearch, dfs::DepthFirstSearch, dijkstra::Dijkstra},
    core::{node::NodeId, value::Value},
    database::layered::LayeredGraphDatabase,
    query::languages::{cypher_lite::CypherLiteLanguage, simple::SimpleQueryLanguage},
};

fn main() {
    println!("═══════════════════════════════════════════════════════");
    println!("  AdGraphDb — Educational Rust Graph Database Demo");
    println!("═══════════════════════════════════════════════════════\n");

    demo_with_json_storage();
    demo_with_binary_storage();
    demo_persistence();
    demo_query_languages();
}

// ── Demo 1: JSON storage adapter ─────────────────────────────────────────────

fn demo_with_json_storage() {
    println!("── Demo 1: JSON file storage ───────────────────────────\n");

    let storage = JsonFileStorage::open("city_graph.json")
        .expect("failed to open JSON storage");

    run_city_graph_demo(storage, "JSON");
}

// ── Demo 2: Binary storage adapter — identical code, different adapter ────────

fn demo_with_binary_storage() {
    println!("\n── Demo 2: Binary file storage (same graph, swapped adapter) ──\n");

    let storage = BinaryFileStorage::open("city_graph.bin")
        .expect("failed to open binary storage");

    run_city_graph_demo(storage, "Binary");
}

// ── Shared graph demo — adapter is erased to Box<dyn StoragePort> ─────────────

fn run_city_graph_demo(
    storage: impl ad_graph_db::ports::storage::StoragePort + 'static,
    label: &str,
) {
    let cache   = LruCache::new(64, 64);
    let engine  = AdjacencyListEngine::new();

    let mut db = LayeredGraphDatabase::open(
        Box::new(storage),
        Box::new(cache),
        Box::new(engine),
    ).expect("failed to open database");

    // ── Insert nodes ──────────────────────────────────────────────────────────

    let london = db.insert_node("City", props([
        ("name", Value::from("London")),
        ("country", Value::from("UK")),
        ("population", Value::from(9_000_000_i64)),
    ])).unwrap();

    let paris = db.insert_node("City", props([
        ("name", Value::from("Paris")),
        ("country", Value::from("France")),
        ("population", Value::from(2_100_000_i64)),
    ])).unwrap();

    let berlin = db.insert_node("City", props([
        ("name", Value::from("Berlin")),
        ("country", Value::from("Germany")),
        ("population", Value::from(3_700_000_i64)),
    ])).unwrap();

    let amsterdam = db.insert_node("City", props([
        ("name", Value::from("Amsterdam")),
        ("country", Value::from("Netherlands")),
        ("population", Value::from(900_000_i64)),
    ])).unwrap();

    let brussels = db.insert_node("City", props([
        ("name", Value::from("Brussels")),
        ("country", Value::from("Belgium")),
        ("population", Value::from(1_200_000_i64)),
    ])).unwrap();

    println!(
        "[{label}] Inserted 5 nodes: London={london}, Paris={paris}, \
        Berlin={berlin}, Amsterdam={amsterdam}, Brussels={brussels}",
    );

    // ── Insert edges (approximate rail distances in km) ────────────────────────

    db.insert_edge(london, paris,     "RAIL", 457.0, HashMap::new()).unwrap();
    db.insert_edge(london, brussels,  "RAIL", 370.0, HashMap::new()).unwrap();
    db.insert_edge(paris,  brussels,  "RAIL", 265.0, HashMap::new()).unwrap();
    db.insert_edge(paris,  berlin,    "RAIL", 1054.0, HashMap::new()).unwrap();
    db.insert_edge(brussels, amsterdam, "RAIL", 174.0, HashMap::new()).unwrap();
    db.insert_edge(amsterdam, berlin,   "RAIL", 648.0, HashMap::new()).unwrap();

    println!(
        "[{label}] Inserted {} edges\n",
        db.edge_count()
    );

    // ── Read back a node (exercises cache) ────────────────────────────────────

    let node = db.get_node(paris).unwrap().unwrap();
    println!("[{label}] get_node({paris}) = {node}");

    // ── BFS traversal ─────────────────────────────────────────────────────────

    let bfs_order = db.traverse(&BreadthFirstSearch, london);
    print!("[{label}] BFS from {london}: ");
    print_node_ids(&bfs_order);

    // ── DFS traversal ─────────────────────────────────────────────────────────

    let dfs_order = db.traverse(&DepthFirstSearch, london);
    print!("[{label}] DFS from {london}: ");
    print_node_ids(&dfs_order);

    // ── Dijkstra: London → Berlin ─────────────────────────────────────────────

    match db.find_shortest_path(&Dijkstra, london, berlin) {
        Some((path, total_km)) => {
            print!("[{label}] Dijkstra {london}→{berlin} ({total_km:.0} km): ");
            print_node_ids(&path);
        }
        None => println!("[{label}] No path found from {london} to {berlin}"),
    }

    // ── Neighbors ─────────────────────────────────────────────────────────────

    let out = db.neighbors_outgoing(paris);
    println!(
        "[{label}] Outgoing neighbors of {paris}: {:?}",
        out.iter().map(|n| n.node_id.to_string()).collect::<Vec<_>>()
    );

    // ── Compact the WAL ───────────────────────────────────────────────────────

    db.compact().expect("compaction failed");
    println!("[{label}] WAL compacted — file now contains only live records.\n");
}

// ── Demo 3: Persistence — write, drop, reopen, read ──────────────────────────

fn demo_persistence() {
    println!("── Demo 3: Persistence (write → drop → reopen → read) ──\n");

    let persistent_path = "persistent_graph.bin";

    // First session: write two nodes.
    let alice_id;
    let bob_id;
    {
        let storage = BinaryFileStorage::open(persistent_path).unwrap();
        let mut db = LayeredGraphDatabase::open(
            Box::new(storage),
            Box::new(LruCache::new(32, 32)),
            Box::new(AdjacencyListEngine::new()),
        ).unwrap();

        alice_id = db.insert_node("Person", props([("name", Value::from("Alice"))])).unwrap();
        bob_id   = db.insert_node("Person", props([("name", Value::from("Bob"))])).unwrap();
        db.insert_edge(alice_id, bob_id, "KNOWS", 1.0, HashMap::new()).unwrap();

        println!("Session 1: inserted Alice={alice_id}, Bob={bob_id}");
        println!("Session 1: dropping database handle...\n");
        // db is dropped here — File handles are closed.
    }

    // Second session: reopen and read — data must still be there.
    {
        let storage = BinaryFileStorage::open(persistent_path).unwrap();
        let mut db = LayeredGraphDatabase::open(
            Box::new(storage),
            Box::new(LruCache::new(32, 32)),
            Box::new(AdjacencyListEngine::new()),
        ).unwrap();

        println!(
            "Session 2: reopened — found {} node(s), {} edge(s)",
            db.node_count(),
            db.edge_count()
        );

        let alice = db.get_node(alice_id).unwrap();
        let bob   = db.get_node(bob_id).unwrap();
        println!("Session 2: Alice = {:?}", alice.map(|n| n.label));
        println!("Session 2: Bob   = {:?}", bob.map(|n| n.label));

        let neighbors = db.neighbors_outgoing(alice_id);
        println!(
            "Session 2: Alice's outgoing neighbors: {:?}",
            neighbors.iter().map(|n| n.node_id.to_string()).collect::<Vec<_>>()
        );
    }

    // Clean up demo files.
    for file in &["city_graph.json", "city_graph.bin", persistent_path] {
        let _ = std::fs::remove_file(file);
    }

    println!("\nDemo complete.");
}

// ── Demo 4: Query languages ───────────────────────────────────────────────────

fn demo_query_languages() {
    println!("\n── Demo 4: Query languages (SimpleQuery vs CypherLite) ──\n");

    // Build a small in-memory graph (using JSON storage for this demo).
    let storage = JsonFileStorage::open("query_demo.json").unwrap();
    let mut db = LayeredGraphDatabase::open(
        Box::new(storage),
        Box::new(LruCache::new(64, 64)),
        Box::new(AdjacencyListEngine::new()),
    ).unwrap();

    // Insert nodes.
    let london = db.insert_node("City", props([
        ("name",       Value::from("London")),
        ("population", Value::from(9_000_000_i64)),
    ])).unwrap();

    let paris = db.insert_node("City", props([
        ("name",       Value::from("Paris")),
        ("population", Value::from(2_100_000_i64)),
    ])).unwrap();

    let berlin = db.insert_node("City", props([
        ("name",       Value::from("Berlin")),
        ("population", Value::from(3_700_000_i64)),
    ])).unwrap();

    let brussels = db.insert_node("City", props([
        ("name",       Value::from("Brussels")),
        ("population", Value::from(1_200_000_i64)),
    ])).unwrap();

    db.insert_edge(london,   paris,    "RAIL", 457.0,  HashMap::new()).unwrap();
    db.insert_edge(london,   brussels, "RAIL", 370.0,  HashMap::new()).unwrap();
    db.insert_edge(paris,    brussels, "RAIL", 265.0,  HashMap::new()).unwrap();
    db.insert_edge(paris,    berlin,   "RAIL", 1054.0, HashMap::new()).unwrap();
    db.insert_edge(brussels, berlin,   "RAIL", 912.0,  HashMap::new()).unwrap();

    let simple = SimpleQueryLanguage;
    let cypher = CypherLiteLanguage;

    // Each pair shows the same query expressed in both languages.
    let pairs: &[(&str, &str, &str)] = &[
        (
            "All cities",
            "MATCH NODE WHERE label = \"City\"",
            "MATCH (n:City) RETURN n",
        ),
        (
            "Cities with population > 2 million",
            "MATCH NODE WHERE label = \"City\" AND props.population > 2000000",
            "MATCH (n:City) WHERE n.population > 2000000 RETURN n",
        ),
        (
            "All rail connections",
            "MATCH EDGE WHERE label = \"RAIL\"",
            "MATCH ()-[r:RAIL]->() RETURN r",
        ),
        (
            "Short rail legs (< 500 km)",
            "MATCH EDGE WHERE weight < 500",
            "MATCH ()-[r]->() WHERE r.weight < 500 RETURN r",
        ),
        (
            "BFS traversal from London",
            &format!("TRAVERSE BFS FROM {london}"),
            &format!("TRAVERSE BFS FROM {london}"),
        ),
        (
            &format!("Shortest path: London → Berlin"),
            &format!("PATH FROM {london} TO {berlin}"),
            &format!("PATH FROM {london} TO {berlin}"),
        ),
        (
            "Node count",
            "COUNT NODES",
            "COUNT NODES",
        ),
    ];

    for (description, simple_q, cypher_q) in pairs {
        println!("  Query: {description}");

        let r1 = db.execute_query(&simple, simple_q).unwrap();
        println!("    SimpleQuery  »  {simple_q}");
        println!("    Result: {r1}");

        let r2 = db.execute_query(&cypher, cypher_q).unwrap();
        println!("    CypherLite   »  {cypher_q}");
        println!("    Result: {r2}");
    }

    // Point lookup.
    let point_q = format!("GET NODE {paris}");
    let r = db.execute_query(&simple, &point_q).unwrap();
    println!("  Point lookup »  {point_q}");
    println!("  Result: {r}");

    let _ = std::fs::remove_file("query_demo.json");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn props<const N: usize>(pairs: [(&str, Value); N]) -> HashMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn print_node_ids(ids: &[NodeId]) {
    let s: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    println!("{}", s.join(" → "));
}
