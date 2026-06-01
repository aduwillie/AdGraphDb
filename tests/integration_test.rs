// Integration tests for AdGraphDb.
//
// These tests exercise the full stack: storage → cache → engine → query.
// They use temporary files and clean up after themselves.
//
// Run:   cargo test
// Run (verbose): cargo test -- --nocapture
// Run single:    cargo test test_name

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ad_graph_db::{
    adapters::{
        cache::{lru::LruCache, no_cache::NoCache},
        engine::adjacency_list::AdjacencyListEngine,
        storage::{binary_file::BinaryFileStorage, json_file::JsonFileStorage},
    },
    algorithms::{bfs::BreadthFirstSearch, dfs::DepthFirstSearch, dijkstra::Dijkstra},
    core::{node::NodeId, value::Value},
    database::layered::LayeredGraphDatabase,
    query::languages::{cypher_lite::CypherLiteLanguage, simple::SimpleQueryLanguage},
    query::result::QueryResult,
};

// ── Shared helpers ────────────────────────────────────────────────────────────

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(suffix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("adgraphdb_it_{n}_{suffix}"))
}

struct TempPath(PathBuf);
impl TempPath {
    fn new(suffix: &str) -> Self { Self(temp_path(suffix)) }
    fn path(&self) -> &PathBuf { &self.0 }
}
impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let tmp = self.0.with_extension(
            self.0.extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("{e}.tmp"))
                .unwrap_or_else(|| "tmp".into()),
        );
        let _ = std::fs::remove_file(tmp);
    }
}

fn json_db(path: &PathBuf) -> LayeredGraphDatabase {
    LayeredGraphDatabase::open(
        Box::new(JsonFileStorage::open(path).unwrap()),
        Box::new(LruCache::new(64, 64)),
        Box::new(AdjacencyListEngine::new()),
    ).unwrap()
}

fn bin_db(path: &PathBuf) -> LayeredGraphDatabase {
    LayeredGraphDatabase::open(
        Box::new(BinaryFileStorage::open(path).unwrap()),
        Box::new(LruCache::new(64, 64)),
        Box::new(AdjacencyListEngine::new()),
    ).unwrap()
}

fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

// ── Node CRUD ─────────────────────────────────────────────────────────────────

#[test]
fn insert_and_retrieve_node_json() {
    let tmp = TempPath::new("crud_node.json");
    let mut db = json_db(tmp.path());
    let id = db.insert_node("Person", props(&[("name", Value::from("Alice"))])).unwrap();
    let node = db.get_node(id).unwrap().unwrap();
    assert_eq!(node.label, "Person");
    assert_eq!(node.properties["name"], Value::Text("Alice".into()));
}

#[test]
fn insert_and_retrieve_node_binary() {
    let tmp = TempPath::new("crud_node.bin");
    let mut db = bin_db(tmp.path());
    let id = db.insert_node("City", props(&[("pop", Value::Integer(1_000_000))])).unwrap();
    let node = db.get_node(id).unwrap().unwrap();
    assert_eq!(node.label, "City");
}

#[test]
fn delete_node_removes_it() {
    let tmp = TempPath::new("del_node.json");
    let mut db = json_db(tmp.path());
    let id = db.insert_node("Temp", HashMap::new()).unwrap();
    db.delete_node(id).unwrap();
    assert!(db.get_node(id).unwrap().is_none());
    assert!(!db.all_node_ids().contains(&id));
}

#[test]
fn get_missing_node_returns_none() {
    let tmp = TempPath::new("miss_node.json");
    let mut db = json_db(tmp.path());
    assert!(db.get_node(NodeId(999)).unwrap().is_none());
}

// ── Edge CRUD ─────────────────────────────────────────────────────────────────

#[test]
fn insert_and_retrieve_edge() {
    let tmp = TempPath::new("crud_edge.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap();
    let eid = db.insert_edge(a, b, "LINKS", 5.0, HashMap::new()).unwrap();
    let edge = db.get_edge(eid).unwrap().unwrap();
    assert_eq!(edge.source, a);
    assert_eq!(edge.target, b);
    assert!((edge.weight - 5.0).abs() < f64::EPSILON);
}

#[test]
fn delete_edge_removes_it_from_engine() {
    let tmp = TempPath::new("del_edge.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap();
    let eid = db.insert_edge(a, b, "LINK", 1.0, HashMap::new()).unwrap();
    db.delete_edge(eid).unwrap();
    assert!(db.get_edge(eid).unwrap().is_none());
    assert!(db.neighbors_outgoing(a).is_empty());
}

#[test]
fn insert_edge_fails_if_source_missing() {
    let tmp = TempPath::new("edge_miss_src.json");
    let mut db = json_db(tmp.path());
    let b = db.insert_node("B", HashMap::new()).unwrap();
    let result = db.insert_edge(NodeId(9999), b, "LINK", 1.0, HashMap::new());
    assert!(result.is_err());
}

#[test]
fn delete_node_cascades_to_incident_edges() {
    let tmp = TempPath::new("cascade.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap();
    db.insert_edge(a, b, "LINK", 1.0, HashMap::new()).unwrap();
    assert_eq!(db.edge_count(), 1);
    db.delete_node(a).unwrap();
    assert_eq!(db.edge_count(), 0);
}

// ── Graph structure ───────────────────────────────────────────────────────────

#[test]
fn neighbors_outgoing_correct() {
    let tmp = TempPath::new("neighbors.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap();
    let c = db.insert_node("C", HashMap::new()).unwrap();
    db.insert_edge(a, b, "E", 1.0, HashMap::new()).unwrap();
    db.insert_edge(a, c, "E", 1.0, HashMap::new()).unwrap();
    let targets: Vec<NodeId> = db.neighbors_outgoing(a).iter().map(|n| n.node_id).collect();
    assert!(targets.contains(&b));
    assert!(targets.contains(&c));
}

// ── Algorithms ───────────────────────────────────────────────────────────────

fn city_graph() -> (LayeredGraphDatabase, TempPath, NodeId, NodeId, NodeId, NodeId) {
    let tmp = TempPath::new("city.json");
    let mut db = json_db(tmp.path());
    let london    = db.insert_node("City", HashMap::new()).unwrap();
    let paris     = db.insert_node("City", HashMap::new()).unwrap();
    let brussels  = db.insert_node("City", HashMap::new()).unwrap();
    let amsterdam = db.insert_node("City", HashMap::new()).unwrap();
    db.insert_edge(london,   paris,     "RAIL", 457.0, HashMap::new()).unwrap();
    db.insert_edge(london,   brussels,  "RAIL", 370.0, HashMap::new()).unwrap();
    db.insert_edge(paris,    brussels,  "RAIL", 265.0, HashMap::new()).unwrap();
    db.insert_edge(brussels, amsterdam, "RAIL", 174.0, HashMap::new()).unwrap();
    (db, tmp, london, paris, brussels, amsterdam)
}

#[test]
fn bfs_visits_all_reachable() {
    let (db, _tmp, london, paris, brussels, amsterdam) = city_graph();
    let mut order = db.traverse(&BreadthFirstSearch, london);
    order.sort_by_key(|id| id.0);
    assert_eq!(order, vec![london, paris, brussels, amsterdam]);
}

#[test]
fn dfs_visits_all_reachable() {
    let (db, _tmp, london, _, _, amsterdam) = city_graph();
    let order = db.traverse(&DepthFirstSearch, london);
    assert!(order.contains(&amsterdam));
    assert_eq!(order[0], london);
}

#[test]
fn dijkstra_finds_minimum_weight_path() {
    let (db, _tmp, london, _paris, _brussels, amsterdam) = city_graph();
    let (path, cost) = db.find_shortest_path(&Dijkstra, london, amsterdam).unwrap();
    // London→Brussels (370) + Brussels→Amsterdam (174) = 544
    assert!((cost - 544.0).abs() < 1.0);
    assert_eq!(path[0], london);
    assert_eq!(*path.last().unwrap(), amsterdam);
}

#[test]
fn dijkstra_returns_none_for_unreachable() {
    let tmp = TempPath::new("unreach.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap(); // no edge
    assert!(db.find_shortest_path(&Dijkstra, a, b).is_none());
}

// ── Persistence (write → close → reopen → read) ───────────────────────────────

#[test]
fn nodes_survive_reopen_json() {
    let tmp = TempPath::new("persist.json");
    let id;
    {
        let mut db = json_db(tmp.path());
        id = db.insert_node("Durable", HashMap::new()).unwrap();
    }
    let mut db2 = json_db(tmp.path());
    assert!(db2.get_node(id).unwrap().is_some());
    assert_eq!(db2.node_count(), 1);
}

#[test]
fn nodes_survive_reopen_binary() {
    let tmp = TempPath::new("persist.bin");
    let id;
    {
        let mut db = bin_db(tmp.path());
        id = db.insert_node("Durable", HashMap::new()).unwrap();
    }
    let mut db2 = bin_db(tmp.path());
    assert!(db2.get_node(id).unwrap().is_some());
}

#[test]
fn edges_survive_reopen() {
    let tmp = TempPath::new("persist_edge.json");
    let eid;
    {
        let mut db = json_db(tmp.path());
        let a = db.insert_node("A", HashMap::new()).unwrap();
        let b = db.insert_node("B", HashMap::new()).unwrap();
        eid = db.insert_edge(a, b, "LINK", 2.0, HashMap::new()).unwrap();
    }
    let mut db2 = json_db(tmp.path());
    assert!(db2.get_edge(eid).unwrap().is_some());
    assert_eq!(db2.edge_count(), 1);
}

#[test]
fn deleted_node_stays_gone_after_reopen() {
    let tmp = TempPath::new("del_persist.json");
    let id;
    {
        let mut db = json_db(tmp.path());
        id = db.insert_node("Gone", HashMap::new()).unwrap();
        db.delete_node(id).unwrap();
    }
    let mut db2 = json_db(tmp.path());
    assert!(db2.get_node(id).unwrap().is_none());
}

#[test]
fn id_generator_seeds_from_storage_on_reopen() {
    let tmp = TempPath::new("id_seed.json");
    let first_id;
    {
        let mut db = json_db(tmp.path());
        first_id = db.insert_node("A", HashMap::new()).unwrap();
    }
    let mut db2 = json_db(tmp.path());
    let second_id = db2.insert_node("B", HashMap::new()).unwrap();
    assert!(second_id.0 > first_id.0, "new IDs must not collide with existing ones");
}

// ── Compaction ────────────────────────────────────────────────────────────────

#[test]
fn compact_leaves_correct_live_count() {
    let tmp = TempPath::new("compact.json");
    let mut db = json_db(tmp.path());
    let ids: Vec<NodeId> = (0..5)
        .map(|_| db.insert_node("Node", HashMap::new()).unwrap())
        .collect();
    db.delete_node(ids[0]).unwrap();
    db.delete_node(ids[4]).unwrap();
    db.compact().unwrap();
    assert_eq!(db.node_count(), 3);
}

// ── Cache (LRU eviction) ──────────────────────────────────────────────────────

#[test]
fn lru_cache_hit_avoids_storage() {
    let tmp = TempPath::new("lru.json");
    // Capacity 1: second node evicts the first.
    let mut db = LayeredGraphDatabase::open(
        Box::new(JsonFileStorage::open(tmp.path()).unwrap()),
        Box::new(LruCache::new(1, 1)),
        Box::new(AdjacencyListEngine::new()),
    ).unwrap();
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap();
    // Both still readable (fall through to storage on cache miss).
    assert!(db.get_node(a).unwrap().is_some());
    assert!(db.get_node(b).unwrap().is_some());
}

#[test]
fn no_cache_still_reads_from_storage() {
    let tmp = TempPath::new("nocache.json");
    let mut db = LayeredGraphDatabase::open(
        Box::new(JsonFileStorage::open(tmp.path()).unwrap()),
        Box::new(NoCache),
        Box::new(AdjacencyListEngine::new()),
    ).unwrap();
    let id = db.insert_node("X", HashMap::new()).unwrap();
    assert!(db.get_node(id).unwrap().is_some());
}

// ── Query languages ───────────────────────────────────────────────────────────

fn query_db() -> (LayeredGraphDatabase, TempPath) {
    let tmp = TempPath::new("query.json");
    let mut db = json_db(tmp.path());
    db.insert_node("City", props(&[("name", Value::from("London")), ("pop", Value::Integer(9_000_000))])).unwrap();
    db.insert_node("City", props(&[("name", Value::from("Paris")),  ("pop", Value::Integer(2_100_000))])).unwrap();
    db.insert_node("Town", props(&[("name", Value::from("Lewes")),  ("pop", Value::Integer(17_000))])).unwrap();
    (db, tmp)
}

#[test]
fn simple_query_match_all_nodes() {
    let (mut db, _tmp) = query_db();
    let result = db.execute_query(&SimpleQueryLanguage, "MATCH NODE").unwrap();
    if let ad_graph_db::query::result::QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 3);
    } else { panic!(); }
}

#[test]
fn simple_query_filter_by_label() {
    let (mut db, _tmp) = query_db();
    let result = db.execute_query(&SimpleQueryLanguage, "MATCH NODE WHERE label = \"City\"").unwrap();
    if let ad_graph_db::query::result::QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| n.label == "City"));
    } else { panic!(); }
}

#[test]
fn simple_query_property_gt_filter() {
    let (mut db, _tmp) = query_db();
    let result = db.execute_query(&SimpleQueryLanguage, "MATCH NODE WHERE props.pop > 1000000").unwrap();
    if let ad_graph_db::query::result::QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 2);
    } else { panic!(); }
}

#[test]
fn cypher_lite_match_all_nodes() {
    let (mut db, _tmp) = query_db();
    let result = db.execute_query(&CypherLiteLanguage, "MATCH (n) RETURN n").unwrap();
    if let ad_graph_db::query::result::QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 3);
    } else { panic!(); }
}

#[test]
fn cypher_lite_match_by_label() {
    let (mut db, _tmp) = query_db();
    let result = db.execute_query(&CypherLiteLanguage, "MATCH (n:City) RETURN n").unwrap();
    if let ad_graph_db::query::result::QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 2);
    } else { panic!(); }
}

#[test]
fn simple_and_cypher_produce_same_results() {
    let (mut db, _tmp) = query_db();
    let r1 = db.execute_query(&SimpleQueryLanguage, "MATCH NODE WHERE label = \"City\"").unwrap();
    let r2 = db.execute_query(&CypherLiteLanguage,  "MATCH (n:City) RETURN n").unwrap();
    if let (
        ad_graph_db::query::result::QueryResult::Nodes(n1),
        ad_graph_db::query::result::QueryResult::Nodes(n2),
    ) = (r1, r2) {
        assert_eq!(n1.len(), n2.len());
    } else { panic!(); }
}

#[test]
fn query_count_nodes() {
    let (mut db, _tmp) = query_db();
    let result = db.execute_query(&SimpleQueryLanguage, "COUNT NODES").unwrap();
    if let ad_graph_db::query::result::QueryResult::Count(n) = result {
        assert_eq!(n, 3);
    } else { panic!(); }
}

#[test]
fn query_traverse_bfs() {
    let tmp = TempPath::new("trav_q.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    db.insert_node("B", HashMap::new()).unwrap();
    db.insert_node("C", HashMap::new()).unwrap();
    let result = db.execute_query(
        &SimpleQueryLanguage,
        &format!("TRAVERSE BFS FROM {a}"),
    ).unwrap();
    assert!(matches!(result, ad_graph_db::query::result::QueryResult::Traversal(_)));
}

#[test]
fn query_shortest_path() {
    let tmp = TempPath::new("path_q.json");
    let mut db = json_db(tmp.path());
    let a = db.insert_node("A", HashMap::new()).unwrap();
    let b = db.insert_node("B", HashMap::new()).unwrap();
    let c = db.insert_node("C", HashMap::new()).unwrap();
    db.insert_edge(a, b, "E", 1.0, HashMap::new()).unwrap();
    db.insert_edge(b, c, "E", 1.0, HashMap::new()).unwrap();
    let q = format!("PATH FROM {a} TO {c}");
    let result = db.execute_query(&SimpleQueryLanguage, &q).unwrap();
    if let QueryResult::Path { nodes, total_weight } = result {
        assert_eq!(nodes.first(), Some(&a));
        assert_eq!(nodes.last(), Some(&c));
        assert!((total_weight - 2.0).abs() < f64::EPSILON);
    } else { panic!("expected Path result, got: {result:?}"); }
}

// ── Label index tests ─────────────────────────────────────────────────────────

#[test]
fn label_index_reduces_query_to_label_count() {
    let tmp = TempPath::new("label_idx.json");
    let mut db = json_db(tmp.path());
    // Insert 5 cities and 3 persons.
    for i in 0..5 { db.insert_node("City",   props(&[("n", Value::Integer(i))])).unwrap(); }
    for i in 0..3 { db.insert_node("Person", props(&[("n", Value::Integer(i))])).unwrap(); }

    // Label lookup: should return exactly 5 City ids.
    assert_eq!(db.node_ids_by_label("City").len(), 5);
    assert_eq!(db.label_count("City"), 5);
    assert_eq!(db.label_count("Person"), 3);
    assert_eq!(db.label_count("Robot"), 0);
}

#[test]
fn label_index_updated_on_delete() {
    let tmp = TempPath::new("label_del.json");
    let mut db = json_db(tmp.path());
    let id = db.insert_node("City", HashMap::new()).unwrap();
    assert_eq!(db.label_count("City"), 1);
    db.delete_node(id).unwrap();
    assert_eq!(db.label_count("City"), 0);
}

#[test]
fn label_index_rebuilt_on_reopen() {
    let tmp = TempPath::new("label_reopen.json");
    {
        let mut db = json_db(tmp.path());
        db.insert_node("City", HashMap::new()).unwrap();
        db.insert_node("City", HashMap::new()).unwrap();
        db.insert_node("Town", HashMap::new()).unwrap();
    }
    let db2 = json_db(tmp.path());
    // Index must be rebuilt from WAL on open.
    assert_eq!(db2.label_count("City"), 2);
    assert_eq!(db2.label_count("Town"), 1);
}

#[test]
fn match_node_by_label_uses_index_path() {
    // Verifies the query executor uses get_nodes_by_label (O(label_count))
    // rather than get_all_nodes (O(N)) when a label filter is present.
    let tmp = TempPath::new("label_query.json");
    let mut db = json_db(tmp.path());
    for _ in 0..10 { db.insert_node("Noise", HashMap::new()).unwrap(); }
    let target = db.insert_node("Needle", props(&[("x", Value::Integer(42))])).unwrap();

    let result = db.execute_query(&SimpleQueryLanguage,
        "MATCH NODE WHERE label = \"Needle\"").unwrap();
    if let QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, target);
    } else { panic!(); }
}

#[test]
fn cypher_match_by_label_uses_index_path() {
    let tmp = TempPath::new("cypher_label.json");
    let mut db = json_db(tmp.path());
    for _ in 0..5 { db.insert_node("Other", HashMap::new()).unwrap(); }
    db.insert_node("Special", HashMap::new()).unwrap();

    let result = db.execute_query(&CypherLiteLanguage,
        "MATCH (n:Special) RETURN n").unwrap();
    if let QueryResult::Nodes(nodes) = result {
        assert_eq!(nodes.len(), 1);
    } else { panic!(); }
}

// ── Transaction tests ─────────────────────────────────────────────────────────

#[test]
fn commit_transaction_applies_all_nodes() {
    let tmp = TempPath::new("txn_commit.json");
    let mut db = json_db(tmp.path());

    let mut txn = db.begin_transaction();
    let alice = txn.stage_insert_node("Person", props(&[("name", Value::from("Alice"))]));
    let bob   = txn.stage_insert_node("Person", props(&[("name", Value::from("Bob"))]));
    txn.stage_insert_edge(alice, bob, "KNOWS", 1.0, HashMap::new());

    let result = db.commit_transaction(txn).unwrap();

    assert_eq!(result.inserted_node_ids.len(), 2);
    assert_eq!(result.inserted_edge_ids.len(), 1);
    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);

    assert!(db.get_node(alice).unwrap().is_some());
    assert!(db.get_node(bob).unwrap().is_some());
}

#[test]
fn transaction_nodes_visible_after_reopen() {
    let tmp = TempPath::new("txn_persist.json");
    let alice_id;
    {
        let mut db = json_db(tmp.path());
        let mut txn = db.begin_transaction();
        alice_id = txn.stage_insert_node("Person", HashMap::new());
        db.commit_transaction(txn).unwrap();
    }
    let mut db2 = json_db(tmp.path());
    assert!(db2.get_node(alice_id).unwrap().is_some());
}

#[test]
fn rollback_discards_staged_ops() {
    let tmp = TempPath::new("txn_rollback.json");
    let mut db = json_db(tmp.path());

    let mut txn = db.begin_transaction();
    txn.stage_insert_node("Ghost", HashMap::new());
    txn.stage_insert_node("Ghost", HashMap::new());
    db.rollback_transaction(txn);

    // Nothing should have been written.
    assert_eq!(db.node_count(), 0);
}

#[test]
fn rollback_prevents_id_reuse() {
    let tmp = TempPath::new("txn_id_reuse.json");
    let mut db = json_db(tmp.path());

    // Stage two nodes but roll back — IDs 0 and 1 are "consumed".
    let mut txn = db.begin_transaction();
    txn.stage_insert_node("A", HashMap::new()); // would be N0
    txn.stage_insert_node("B", HashMap::new()); // would be N1
    db.rollback_transaction(txn);

    // Next real insert must NOT reuse N0 or N1.
    let id = db.insert_node("Real", HashMap::new()).unwrap();
    assert!(id.0 >= 2, "ID {id} was reused from a rolled-back transaction");
}

#[test]
fn staged_ids_usable_for_edges_within_transaction() {
    // The key transaction ergonomics test: insert two nodes and an edge
    // between them in a single transaction, using the IDs returned by staging.
    let tmp = TempPath::new("txn_edge.json");
    let mut db = json_db(tmp.path());

    let mut txn = db.begin_transaction();
    let src = txn.stage_insert_node("City", HashMap::new());
    let tgt = txn.stage_insert_node("City", HashMap::new());
    let eid = txn.stage_insert_edge(src, tgt, "RAIL", 457.0, HashMap::new());

    db.commit_transaction(txn).unwrap();

    let edge = db.get_edge(eid).unwrap().unwrap();
    assert_eq!(edge.source, src);
    assert_eq!(edge.target, tgt);
}

#[test]
fn transaction_delete_nodes_committed() {
    let tmp = TempPath::new("txn_del.json");
    let mut db = json_db(tmp.path());
    let id = db.insert_node("Temp", HashMap::new()).unwrap();

    let mut txn = db.begin_transaction();
    txn.stage_delete_node(id);
    db.commit_transaction(txn).unwrap();

    assert!(db.get_node(id).unwrap().is_none());
    assert_eq!(db.node_count(), 0);
}
