// SimpleQueryLanguage — a straightforward keyword-based graph query DSL.
//
// ── Grammar ───────────────────────────────────────────────────────────────────
//
//   query        = match_cmd | get_cmd | traverse_cmd | path_cmd | count_cmd
//
//   match_cmd    = "MATCH" ("NODE" | "EDGE") ["WHERE" filter_chain]
//   get_cmd      = "GET"   ("NODE" node_ref | "EDGE" edge_ref)
//   traverse_cmd = "TRAVERSE" ("BFS" | "DFS") "FROM" node_ref
//   path_cmd     = "PATH" "FROM" node_ref "TO" node_ref
//   count_cmd    = "COUNT" ("NODES" | "EDGES")
//
//   filter_chain = condition {"AND" condition}
//
//   condition    = "label"  op  string_lit
//                | "weight" op  number_lit
//                | "props." ident op value_lit
//
//   op           = "=" | "!=" | "<" | "<=" | ">" | ">="
//
//   node_ref     = /N\d+/     (e.g. N0, N42)
//   edge_ref     = /E\d+/     (e.g. E0, E7)
//   string_lit   = "..." (double-quoted)
//   number_lit   = integer or float
//   bool_lit     = "true" | "false"
//
// ── Example queries ───────────────────────────────────────────────────────────
//
//   MATCH NODE
//   MATCH NODE WHERE label = "City"
//   MATCH NODE WHERE label = "City" AND props.population > 1000000
//   MATCH EDGE WHERE label = "RAIL"
//   MATCH EDGE WHERE weight < 500.0
//   GET NODE N0
//   GET EDGE E2
//   TRAVERSE BFS FROM N0
//   TRAVERSE DFS FROM N0
//   PATH FROM N0 TO N4
//   COUNT NODES
//   COUNT EDGES

use crate::core::{
    edge::EdgeId,
    error::GraphError,
    node::NodeId,
    value::Value,
};
use crate::ports::query_context::DatabaseContext;
use crate::query::{
    ast::{
        ComparisonOp, EdgeFilter, NodeFilter, PropertyCondition, QueryCommand, TraversalKind,
        WeightCondition,
    },
    executor,
    port::QueryLanguagePort,
    result::QueryResult,
};

// ── Public adapter struct ─────────────────────────────────────────────────────

pub struct SimpleQueryLanguage;

impl QueryLanguagePort for SimpleQueryLanguage {
    fn language_name(&self) -> &str { "SimpleQuery" }

    fn execute(
        &self,
        query: &str,
        context: &mut dyn DatabaseContext,
    ) -> Result<QueryResult, GraphError> {
        let command = Parser::new(query).parse()?;
        executor::execute(command, context)
    }
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────
//
// SimpleQuery is whitespace-delimited, so we split first and then refine each
// token.  The only complication is quoted strings which may contain spaces —
// we handle those with a simple state machine before splitting.

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_string = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                current.push(ch);
                if in_string {
                    // Closing quote — emit the complete string token.
                    tokens.push(current.clone());
                    current.clear();
                }
                in_string = !in_string;
            }
            ' ' | '\t' | '\n' | '\r' if !in_string => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

// ── Parser ────────────────────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<String>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self { tokens: tokenize(input), pos: 0 }
    }

    // ── Peek / consume helpers ────────────────────────────────────────────────

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|s| s.as_str())
    }

    fn next(&mut self) -> Option<&str> {
        let tok = self.tokens.get(self.pos).map(|s| s.as_str());
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &str) -> Result<(), GraphError> {
        match self.next() {
            Some(tok) if tok.eq_ignore_ascii_case(expected) => Ok(()),
            Some(tok) => Err(parse_error(format!(
                "expected '{expected}' but got '{tok}'"
            ))),
            None => Err(parse_error(format!("expected '{expected}' but reached end of input"))),
        }
    }

    fn keyword(&mut self) -> Result<String, GraphError> {
        self.next()
            .map(|s| s.to_uppercase())
            .ok_or_else(|| parse_error("unexpected end of input".into()))
    }

    // ── Top-level dispatch ────────────────────────────────────────────────────

    fn parse(&mut self) -> Result<QueryCommand, GraphError> {
        let kw = self.keyword()?;
        match kw.as_str() {
            "MATCH"    => self.parse_match(),
            "GET"      => self.parse_get(),
            "TRAVERSE" => self.parse_traverse(),
            "PATH"     => self.parse_path(),
            "COUNT"    => self.parse_count(),
            other => Err(parse_error(format!(
                "unknown command '{other}' — expected MATCH, GET, TRAVERSE, PATH, or COUNT"
            ))),
        }
    }

    // ── MATCH ─────────────────────────────────────────────────────────────────

    fn parse_match(&mut self) -> Result<QueryCommand, GraphError> {
        let kind = self.keyword()?;
        match kind.as_str() {
            "NODE" => {
                let filter = self.parse_optional_node_filter()?;
                Ok(QueryCommand::MatchNodes(filter))
            }
            "EDGE" => {
                let filter = self.parse_optional_edge_filter()?;
                Ok(QueryCommand::MatchEdges(filter))
            }
            other => Err(parse_error(format!("expected NODE or EDGE after MATCH, got '{other}'"))),
        }
    }

    fn parse_optional_node_filter(&mut self) -> Result<NodeFilter, GraphError> {
        if !self.peek_keyword("WHERE") {
            return Ok(NodeFilter::default());
        }
        self.next(); // consume WHERE
        self.parse_node_filter_chain()
    }

    fn parse_node_filter_chain(&mut self) -> Result<NodeFilter, GraphError> {
        let mut filter = NodeFilter::default();

        loop {
            self.apply_node_condition(&mut filter)?;
            if self.peek_keyword("AND") {
                self.next(); // consume AND
            } else {
                break;
            }
        }

        Ok(filter)
    }

    fn apply_node_condition(&mut self, filter: &mut NodeFilter) -> Result<(), GraphError> {
        let field = self.next().ok_or_else(|| parse_error("expected condition field".into()))?;

        if field.eq_ignore_ascii_case("label") {
            let op = self.parse_op()?;
            if op != ComparisonOp::Eq {
                return Err(parse_error("label only supports = comparison".into()));
            }
            filter.label = Some(self.parse_string_lit()?);
        } else if field.to_lowercase().starts_with("props.") {
            let key = field["props.".len()..].to_string();
            let op = self.parse_op()?;
            let value = self.parse_value_lit()?;
            filter.property_conditions.push(PropertyCondition { key, op, value });
        } else {
            return Err(parse_error(format!(
                "unknown node filter field '{field}' — use 'label' or 'props.<key>'"
            )));
        }

        Ok(())
    }

    fn parse_optional_edge_filter(&mut self) -> Result<EdgeFilter, GraphError> {
        if !self.peek_keyword("WHERE") {
            return Ok(EdgeFilter::default());
        }
        self.next(); // consume WHERE
        self.parse_edge_filter_chain()
    }

    fn parse_edge_filter_chain(&mut self) -> Result<EdgeFilter, GraphError> {
        let mut filter = EdgeFilter::default();

        loop {
            self.apply_edge_condition(&mut filter)?;
            if self.peek_keyword("AND") {
                self.next();
            } else {
                break;
            }
        }

        Ok(filter)
    }

    fn apply_edge_condition(&mut self, filter: &mut EdgeFilter) -> Result<(), GraphError> {
        let field = self.next().ok_or_else(|| parse_error("expected condition field".into()))?;

        if field.eq_ignore_ascii_case("label") {
            let op = self.parse_op()?;
            if op != ComparisonOp::Eq {
                return Err(parse_error("label only supports = comparison".into()));
            }
            filter.label = Some(self.parse_string_lit()?);
        } else if field.eq_ignore_ascii_case("weight") {
            let op = self.parse_op()?;
            let raw = self.next().ok_or_else(|| parse_error("expected number after weight op".into()))?;
            let value: f64 = raw.parse().map_err(|_| parse_error(format!("'{raw}' is not a number")))?;
            filter.weight_condition = Some(WeightCondition { op, value });
        } else if field.to_lowercase().starts_with("props.") {
            let key = field["props.".len()..].to_string();
            let op = self.parse_op()?;
            let value = self.parse_value_lit()?;
            filter.property_conditions.push(PropertyCondition { key, op, value });
        } else {
            return Err(parse_error(format!(
                "unknown edge filter field '{field}' — use 'label', 'weight', or 'props.<key>'"
            )));
        }

        Ok(())
    }

    // ── GET ───────────────────────────────────────────────────────────────────

    fn parse_get(&mut self) -> Result<QueryCommand, GraphError> {
        let kind = self.keyword()?;
        match kind.as_str() {
            "NODE" => Ok(QueryCommand::GetNode(self.parse_node_ref()?)),
            "EDGE" => Ok(QueryCommand::GetEdge(self.parse_edge_ref()?)),
            other => Err(parse_error(format!("expected NODE or EDGE after GET, got '{other}'"))),
        }
    }

    // ── TRAVERSE ──────────────────────────────────────────────────────────────

    fn parse_traverse(&mut self) -> Result<QueryCommand, GraphError> {
        let algo = self.keyword()?;
        let kind = match algo.as_str() {
            "BFS" => TraversalKind::Bfs,
            "DFS" => TraversalKind::Dfs,
            other => return Err(parse_error(format!("expected BFS or DFS, got '{other}'"))),
        };
        self.expect("FROM")?;
        let start = self.parse_node_ref()?;
        Ok(QueryCommand::Traverse { kind, start })
    }

    // ── PATH ──────────────────────────────────────────────────────────────────

    fn parse_path(&mut self) -> Result<QueryCommand, GraphError> {
        self.expect("FROM")?;
        let start = self.parse_node_ref()?;
        self.expect("TO")?;
        let goal = self.parse_node_ref()?;
        Ok(QueryCommand::ShortestPath { start, goal })
    }

    // ── COUNT ─────────────────────────────────────────────────────────────────

    fn parse_count(&mut self) -> Result<QueryCommand, GraphError> {
        let what = self.keyword()?;
        match what.as_str() {
            "NODES" => Ok(QueryCommand::CountNodes),
            "EDGES" => Ok(QueryCommand::CountEdges),
            other => Err(parse_error(format!("expected NODES or EDGES after COUNT, got '{other}'"))),
        }
    }

    // ── Primitive parsers ─────────────────────────────────────────────────────

    fn parse_op(&mut self) -> Result<ComparisonOp, GraphError> {
        let tok = self.next().ok_or_else(|| parse_error("expected comparison operator".into()))?;
        match tok {
            "="  => Ok(ComparisonOp::Eq),
            "!=" => Ok(ComparisonOp::NotEq),
            "<"  => Ok(ComparisonOp::Lt),
            "<=" => Ok(ComparisonOp::LtEq),
            ">"  => Ok(ComparisonOp::Gt),
            ">=" => Ok(ComparisonOp::GtEq),
            other => Err(parse_error(format!("unknown operator '{other}'"))),
        }
    }

    fn parse_string_lit(&mut self) -> Result<String, GraphError> {
        let tok = self.next().ok_or_else(|| parse_error("expected string literal".into()))?;
        if tok.starts_with('"') && tok.ends_with('"') && tok.len() >= 2 {
            Ok(tok[1..tok.len() - 1].to_string())
        } else {
            Err(parse_error(format!("expected a quoted string, got '{tok}'")))
        }
    }

    fn parse_value_lit(&mut self) -> Result<Value, GraphError> {
        let tok = self.next().ok_or_else(|| parse_error("expected a value".into()))?;

        if tok.starts_with('"') {
            // String literal (already includes the quotes from tokenizer).
            if tok.ends_with('"') && tok.len() >= 2 {
                return Ok(Value::Text(tok[1..tok.len() - 1].to_string()));
            }
            return Err(parse_error(format!("unterminated string literal: {tok}")));
        }
        if tok.eq_ignore_ascii_case("true")  { return Ok(Value::Boolean(true)); }
        if tok.eq_ignore_ascii_case("false") { return Ok(Value::Boolean(false)); }
        if tok.eq_ignore_ascii_case("null")  { return Ok(Value::Null); }

        if tok.contains('.') {
            let f: f64 = tok.parse().map_err(|_| parse_error(format!("invalid float '{tok}'")))?;
            return Ok(Value::Float(f));
        }

        let i: i64 = tok.parse().map_err(|_| parse_error(format!("invalid number '{tok}'")))?;
        Ok(Value::Integer(i))
    }

    fn parse_node_ref(&mut self) -> Result<NodeId, GraphError> {
        let tok = self.next().ok_or_else(|| parse_error("expected node reference (e.g. N0)".into()))?;
        tok.strip_prefix('N')
            .and_then(|rest| rest.parse::<u64>().ok())
            .map(NodeId)
            .ok_or_else(|| parse_error(format!("'{tok}' is not a valid node reference — expected N<number>")))
    }

    fn parse_edge_ref(&mut self) -> Result<EdgeId, GraphError> {
        let tok = self.next().ok_or_else(|| parse_error("expected edge reference (e.g. E0)".into()))?;
        tok.strip_prefix('E')
            .and_then(|rest| rest.parse::<u64>().ok())
            .map(EdgeId)
            .ok_or_else(|| parse_error(format!("'{tok}' is not a valid edge reference — expected E<number>")))
    }

    fn peek_keyword(&self, kw: &str) -> bool {
        self.peek()
            .map(|t| t.eq_ignore_ascii_case(kw))
            .unwrap_or(false)
    }
}

fn parse_error(msg: String) -> GraphError {
    GraphError::DeserializationError(format!("[SimpleQuery] {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{ComparisonOp, QueryCommand, TraversalKind};

    fn parse(q: &str) -> QueryCommand {
        Parser::new(q).parse().unwrap_or_else(|e| panic!("Parse failed: {e}"))
    }

    fn parse_err(q: &str) -> String {
        Parser::new(q).parse().unwrap_err().to_string()
    }

    #[test]
    fn match_all_nodes() {
        assert!(matches!(parse("MATCH NODE"), QueryCommand::MatchNodes(_)));
    }

    #[test]
    fn match_node_with_label_filter() {
        if let QueryCommand::MatchNodes(f) = parse("MATCH NODE WHERE label = \"City\"") {
            assert_eq!(f.label.as_deref(), Some("City"));
        } else { panic!("wrong variant"); }
    }

    #[test]
    fn match_node_with_property_gt() {
        if let QueryCommand::MatchNodes(f) = parse("MATCH NODE WHERE props.pop > 1000") {
            assert_eq!(f.property_conditions[0].key, "pop");
            assert_eq!(f.property_conditions[0].op, ComparisonOp::Gt);
            assert_eq!(f.property_conditions[0].value, Value::Integer(1000));
        } else { panic!(); }
    }

    #[test]
    fn match_node_label_and_property() {
        if let QueryCommand::MatchNodes(f) =
            parse("MATCH NODE WHERE label = \"City\" AND props.pop > 0")
        {
            assert!(f.label.is_some());
            assert_eq!(f.property_conditions.len(), 1);
        } else { panic!(); }
    }

    #[test]
    fn match_all_edges() {
        assert!(matches!(parse("MATCH EDGE"), QueryCommand::MatchEdges(_)));
    }

    #[test]
    fn match_edge_by_label() {
        if let QueryCommand::MatchEdges(f) = parse("MATCH EDGE WHERE label = \"RAIL\"") {
            assert_eq!(f.label.as_deref(), Some("RAIL"));
        } else { panic!(); }
    }

    #[test]
    fn match_edge_weight_lt() {
        if let QueryCommand::MatchEdges(f) = parse("MATCH EDGE WHERE weight < 500") {
            let wc = f.weight_condition.unwrap();
            assert_eq!(wc.op, ComparisonOp::Lt);
            assert!((wc.value - 500.0).abs() < f64::EPSILON);
        } else { panic!(); }
    }

    #[test]
    fn get_node_parses_ref() {
        assert!(matches!(parse("GET NODE N7"), QueryCommand::GetNode(NodeId(7))));
    }

    #[test]
    fn get_edge_parses_ref() {
        assert!(matches!(parse("GET EDGE E3"), QueryCommand::GetEdge(EdgeId(3))));
    }

    #[test]
    fn traverse_bfs() {
        if let QueryCommand::Traverse { kind, start } = parse("TRAVERSE BFS FROM N0") {
            assert_eq!(kind, TraversalKind::Bfs);
            assert_eq!(start, NodeId(0));
        } else { panic!(); }
    }

    #[test]
    fn traverse_dfs() {
        if let QueryCommand::Traverse { kind, .. } = parse("TRAVERSE DFS FROM N0") {
            assert_eq!(kind, TraversalKind::Dfs);
        } else { panic!(); }
    }

    #[test]
    fn path_command() {
        if let QueryCommand::ShortestPath { start, goal } = parse("PATH FROM N0 TO N5") {
            assert_eq!(start, NodeId(0));
            assert_eq!(goal, NodeId(5));
        } else { panic!(); }
    }

    #[test]
    fn count_nodes() {
        assert!(matches!(parse("COUNT NODES"), QueryCommand::CountNodes));
    }

    #[test]
    fn count_edges() {
        assert!(matches!(parse("COUNT EDGES"), QueryCommand::CountEdges));
    }

    #[test]
    fn unknown_command_returns_error() {
        assert!(parse_err("EXPLODE EVERYTHING").contains("SimpleQuery"));
    }

    #[test]
    fn case_insensitive_keywords() {
        assert!(matches!(parse("match node"), QueryCommand::MatchNodes(_)));
        assert!(matches!(parse("count nodes"), QueryCommand::CountNodes));
    }

    #[test]
    fn quoted_string_with_space_is_single_token() {
        if let QueryCommand::MatchNodes(f) = parse("MATCH NODE WHERE label = \"Big City\"") {
            assert_eq!(f.label.as_deref(), Some("Big City"));
        } else { panic!(); }
    }
}
