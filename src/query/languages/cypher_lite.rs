// CypherLiteLanguage — a Cypher-inspired query DSL.
//
// Demonstrates how a completely different surface syntax produces the same
// QueryCommand IR and therefore works with the same executor — showing why
// the language/executor split matters.
//
// ── Grammar ───────────────────────────────────────────────────────────────────
//
//   query           = match_clause | traverse_clause | path_clause | count_clause
//
//   match_clause    = "MATCH" pattern ["WHERE" where_expr] "RETURN" return_list
//
//   pattern         = node_pat ["-[" rel_pat "]->" node_pat]
//   node_pat        = "(" [var] [":" label] ")"
//   rel_pat         = [var] [":" label]
//
//   where_expr      = where_cond {"AND" where_cond}
//   where_cond      = var "." prop op value_lit
//
//   traverse_clause = "TRAVERSE" ("BFS"|"DFS") "FROM" node_ref
//   path_clause     = "PATH" "FROM" node_ref "TO" node_ref
//   count_clause    = "COUNT" ("NODES"|"EDGES")
//
//   node_ref        = /N\d+/
//   var             = identifier
//   label           = identifier
//   op              = "=" | "!=" | "<" | "<=" | ">" | ">="
//
// ── Example queries ───────────────────────────────────────────────────────────
//
//   MATCH (n) RETURN n
//   MATCH (n:City) RETURN n
//   MATCH (n:City) WHERE n.population > 1000000 RETURN n
//   MATCH (n) WHERE n.name = "London" RETURN n
//   MATCH ()-[r:RAIL]->() RETURN r
//   MATCH ()-[r]->() WHERE r.weight < 500 RETURN r
//   TRAVERSE BFS FROM N0
//   TRAVERSE DFS FROM N0
//   PATH FROM N0 TO N4
//   COUNT NODES
//   COUNT EDGES

use crate::core::{error::GraphError, node::NodeId, value::Value};
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

pub struct CypherLiteLanguage;

impl QueryLanguagePort for CypherLiteLanguage {
    fn language_name(&self) -> &str { "CypherLite" }

    fn execute(
        &self,
        query: &str,
        context: &mut dyn DatabaseContext,
    ) -> Result<QueryResult, GraphError> {
        let command = Parser::new(query).parse()?;
        executor::execute(command, context)
    }
}

// ── Token types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Keywords (case-insensitive — stored as uppercase)
    Keyword(String),
    // Identifiers / variable names
    Ident(String),
    // A node reference like N0, N42
    NodeRef(u64),
    // Literals
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    // Operators
    Eq, NotEq, Lt, LtEq, Gt, GtEq,
    // Punctuation
    LParen,   // (
    RParen,   // )
    LBracket, // [
    RBracket, // ]
    Colon,    // :
    Dot,      // .
    Comma,    // ,
    Dash,     // -
    Arrow,    // ->
}

// ── Lexer ─────────────────────────────────────────────────────────────────────

const KEYWORDS: &[&str] = &[
    "MATCH", "WHERE", "RETURN", "AND", "TRAVERSE", "PATH",
    "FROM", "TO", "COUNT", "NODES", "EDGES", "BFS", "DFS",
];

fn lex(input: &str) -> Result<Vec<Token>, GraphError> {
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0;
    let mut tokens = Vec::new();

    while pos < chars.len() {
        // Skip whitespace.
        if chars[pos].is_whitespace() { pos += 1; continue; }

        // Quoted string.
        if chars[pos] == '"' {
            pos += 1;
            let start = pos;
            while pos < chars.len() && chars[pos] != '"' { pos += 1; }
            if pos >= chars.len() {
                return Err(lex_error("unterminated string literal".into()));
            }
            let s: String = chars[start..pos].iter().collect();
            tokens.push(Token::StringLit(s));
            pos += 1; // consume closing "
            continue;
        }

        // Two-char operators.
        if pos + 1 < chars.len() {
            let two: String = chars[pos..pos + 2].iter().collect();
            match two.as_str() {
                "!=" => { tokens.push(Token::NotEq); pos += 2; continue; }
                "<=" => { tokens.push(Token::LtEq); pos += 2; continue; }
                ">=" => { tokens.push(Token::GtEq); pos += 2; continue; }
                "->" => { tokens.push(Token::Arrow); pos += 2; continue; }
                _ => {}
            }
        }

        // Single-char tokens.
        match chars[pos] {
            '(' => { tokens.push(Token::LParen);   pos += 1; continue; }
            ')' => { tokens.push(Token::RParen);   pos += 1; continue; }
            '[' => { tokens.push(Token::LBracket); pos += 1; continue; }
            ']' => { tokens.push(Token::RBracket); pos += 1; continue; }
            ':' => { tokens.push(Token::Colon);    pos += 1; continue; }
            '.' => { tokens.push(Token::Dot);      pos += 1; continue; }
            ',' => { tokens.push(Token::Comma);    pos += 1; continue; }
            '-' => { tokens.push(Token::Dash);     pos += 1; continue; }
            '=' => { tokens.push(Token::Eq);       pos += 1; continue; }
            '<' => { tokens.push(Token::Lt);       pos += 1; continue; }
            '>' => { tokens.push(Token::Gt);       pos += 1; continue; }
            _ => {}
        }

        // Numbers.
        if chars[pos].is_ascii_digit() || (chars[pos] == '-' && pos + 1 < chars.len() && chars[pos + 1].is_ascii_digit()) {
            let start = pos;
            if chars[pos] == '-' { pos += 1; }
            while pos < chars.len() && chars[pos].is_ascii_digit() { pos += 1; }
            let is_float = pos < chars.len() && chars[pos] == '.';
            if is_float {
                pos += 1;
                while pos < chars.len() && chars[pos].is_ascii_digit() { pos += 1; }
                let s: String = chars[start..pos].iter().collect();
                let f: f64 = s.parse().map_err(|_| lex_error(format!("invalid float '{s}'")))?;
                tokens.push(Token::FloatLit(f));
            } else {
                let s: String = chars[start..pos].iter().collect();
                let i: i64 = s.parse().map_err(|_| lex_error(format!("invalid integer '{s}'")))?;
                tokens.push(Token::IntLit(i));
            }
            continue;
        }

        // Identifiers, keywords, node refs (N0, N42), boolean literals.
        if chars[pos].is_alphabetic() || chars[pos] == '_' {
            let start = pos;
            while pos < chars.len() && (chars[pos].is_alphanumeric() || chars[pos] == '_') {
                pos += 1;
            }
            let word: String = chars[start..pos].iter().collect();

            // Node reference: starts with N followed by digits.
            if word.starts_with('N') && word.len() > 1 && word[1..].chars().all(|c| c.is_ascii_digit()) {
                let id: u64 = word[1..].parse().unwrap();
                tokens.push(Token::NodeRef(id));
                continue;
            }

            match word.to_uppercase().as_str() {
                "TRUE"  => { tokens.push(Token::BoolLit(true));  continue; }
                "FALSE" => { tokens.push(Token::BoolLit(false)); continue; }
                up if KEYWORDS.contains(&up) => {
                    tokens.push(Token::Keyword(up.to_string()));
                    continue;
                }
                _ => {
                    tokens.push(Token::Ident(word));
                    continue;
                }
            }
        }

        return Err(lex_error(format!("unexpected character '{}'", chars[pos])));
    }

    Ok(tokens)
}

// ── Parser ────────────────────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        // Lex eagerly; store errors and surface them in parse().
        let tokens = lex(input).unwrap_or_default();
        Self { tokens, pos: 0 }
    }

    fn new_result(input: &str) -> Result<Self, GraphError> {
        Ok(Self { tokens: lex(input)?, pos: 0 })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        tok
    }

    fn expect_keyword(&mut self, kw: &str) -> Result<(), GraphError> {
        match self.next() {
            Some(Token::Keyword(k)) if k == kw => Ok(()),
            Some(other) => Err(parse_error(format!("expected keyword '{kw}', got {other:?}"))),
            None => Err(parse_error(format!("expected keyword '{kw}' but reached end of input"))),
        }
    }

    fn peek_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Token::Keyword(k)) if k == kw)
    }

    // ── Top-level ─────────────────────────────────────────────────────────────

    fn parse(&mut self) -> Result<QueryCommand, GraphError> {
        match self.peek() {
            Some(Token::Keyword(k)) if k == "MATCH"    => self.parse_match(),
            Some(Token::Keyword(k)) if k == "TRAVERSE" => self.parse_traverse(),
            Some(Token::Keyword(k)) if k == "PATH"     => self.parse_path(),
            Some(Token::Keyword(k)) if k == "COUNT"    => self.parse_count(),
            Some(other) => Err(parse_error(format!(
                "expected MATCH, TRAVERSE, PATH, or COUNT at start of query, got {other:?}"
            ))),
            None => Err(parse_error("empty query".into())),
        }
    }

    // ── MATCH ─────────────────────────────────────────────────────────────────

    fn parse_match(&mut self) -> Result<QueryCommand, GraphError> {
        self.expect_keyword("MATCH")?;

        // Determine pattern shape: node-only or relationship pattern.
        // node_only:  (var[:label])
        // rel_pattern: (var[:label])-[rel[:label]]->(var[:label])
        let is_rel_pattern = self.is_relationship_pattern();

        if is_rel_pattern {
            self.parse_rel_match()
        } else {
            self.parse_node_match()
        }
    }

    /// Lookahead: is the pattern `(...)-[...]->(...)` ?
    fn is_relationship_pattern(&self) -> bool {
        // Scan forward for a '-[' sequence after the first node pattern.
        let mut depth = 0;
        let mut found_close = false;
        for tok in &self.tokens[self.pos..] {
            match tok {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 { found_close = true; }
                }
                Token::Dash if found_close => return true,
                Token::Keyword(k) if k == "WHERE" || k == "RETURN" => return false,
                _ => {}
            }
        }
        false
    }

    fn parse_node_match(&mut self) -> Result<QueryCommand, GraphError> {
        // Parse (var[:label])
        let (_, label) = self.parse_node_pattern()?;

        // Optional WHERE clause builds a NodeFilter.
        let mut filter = NodeFilter::default();
        filter.label = label;

        if self.peek_keyword("WHERE") {
            self.next(); // consume WHERE
            self.apply_node_where_conditions(&mut filter)?;
        }

        self.expect_keyword("RETURN")?;
        self.skip_return_list();

        Ok(QueryCommand::MatchNodes(filter))
    }

    fn parse_rel_match(&mut self) -> Result<QueryCommand, GraphError> {
        // Pattern: ()-[r[:label]]->(m[:label])  or  ()-[r]->()
        let _src = self.parse_node_pattern()?;

        // Consume -[
        self.expect_dash()?;
        self.expect_token(Token::LBracket)?;

        // Relationship variable and optional label.
        let rel_label = self.parse_rel_pattern()?;

        // Consume ]->
        self.expect_token(Token::RBracket)?;
        self.expect_token(Token::Arrow)?;

        let _dst = self.parse_node_pattern()?;

        // Build edge filter.
        let mut filter = EdgeFilter::default();
        filter.label = rel_label;

        if self.peek_keyword("WHERE") {
            self.next(); // consume WHERE
            self.apply_edge_where_conditions(&mut filter)?;
        }

        self.expect_keyword("RETURN")?;
        self.skip_return_list();

        Ok(QueryCommand::MatchEdges(filter))
    }

    /// Parse `(var[:label])` — returns (variable_name, label).
    fn parse_node_pattern(&mut self) -> Result<(Option<String>, Option<String>), GraphError> {
        self.expect_token(Token::LParen)?;
        let var = match self.peek() {
            Some(Token::Ident(_)) => {
                if let Some(Token::Ident(name)) = self.next() { Some(name) } else { None }
            }
            _ => None,
        };
        let label = if matches!(self.peek(), Some(Token::Colon)) {
            self.next(); // consume :
            match self.next() {
                Some(Token::Ident(l)) => Some(l),
                _ => return Err(parse_error("expected label after ':'".into())),
            }
        } else {
            None
        };
        self.expect_token(Token::RParen)?;
        Ok((var, label))
    }

    /// Parse the content inside `[...]` — optional var, optional :label.
    fn parse_rel_pattern(&mut self) -> Result<Option<String>, GraphError> {
        // Skip optional variable name.
        if matches!(self.peek(), Some(Token::Ident(_))) {
            self.next();
        }
        // Optional :label.
        if matches!(self.peek(), Some(Token::Colon)) {
            self.next(); // consume :
            return match self.next() {
                Some(Token::Ident(l)) => Ok(Some(l)),
                _ => Err(parse_error("expected relationship label after ':'".into())),
            };
        }
        Ok(None)
    }

    fn apply_node_where_conditions(&mut self, filter: &mut NodeFilter) -> Result<(), GraphError> {
        loop {
            // Expect: var "." field op value
            let _var = self.consume_ident_or_any()?;
            self.expect_token(Token::Dot)?;
            let field = self.consume_ident()?;
            let op = self.parse_op()?;
            let value = self.parse_value_lit()?;

            filter.property_conditions.push(PropertyCondition { key: field, op, value });

            if self.peek_keyword("AND") {
                self.next();
            } else {
                break;
            }
        }
        Ok(())
    }

    fn apply_edge_where_conditions(&mut self, filter: &mut EdgeFilter) -> Result<(), GraphError> {
        loop {
            let _var = self.consume_ident_or_any()?;
            self.expect_token(Token::Dot)?;
            let field = self.consume_ident()?;
            let op = self.parse_op()?;
            let value = self.parse_value_lit()?;

            if field.eq_ignore_ascii_case("weight") {
                let w = match &value {
                    Value::Float(f) => *f,
                    Value::Integer(i) => *i as f64,
                    _ => return Err(parse_error("weight must be a number".into())),
                };
                filter.weight_condition = Some(WeightCondition { op, value: w });
            } else {
                filter.property_conditions.push(PropertyCondition { key: field, op, value });
            }

            if self.peek_keyword("AND") {
                self.next();
            } else {
                break;
            }
        }
        Ok(())
    }

    fn skip_return_list(&mut self) {
        // Consume tokens until we hit a keyword or end of input.
        loop {
            match self.peek() {
                Some(Token::Keyword(_)) | None => break,
                _ => { self.next(); }
            }
        }
    }

    // ── TRAVERSE ──────────────────────────────────────────────────────────────

    fn parse_traverse(&mut self) -> Result<QueryCommand, GraphError> {
        self.expect_keyword("TRAVERSE")?;
        let algo = match self.next() {
            Some(Token::Keyword(k)) if k == "BFS" => TraversalKind::Bfs,
            Some(Token::Keyword(k)) if k == "DFS" => TraversalKind::Dfs,
            other => return Err(parse_error(format!("expected BFS or DFS, got {other:?}"))),
        };
        self.expect_keyword("FROM")?;
        let start = self.parse_node_ref()?;
        Ok(QueryCommand::Traverse { kind: algo, start })
    }

    // ── PATH ──────────────────────────────────────────────────────────────────

    fn parse_path(&mut self) -> Result<QueryCommand, GraphError> {
        self.expect_keyword("PATH")?;
        self.expect_keyword("FROM")?;
        let start = self.parse_node_ref()?;
        self.expect_keyword("TO")?;
        let goal = self.parse_node_ref()?;
        Ok(QueryCommand::ShortestPath { start, goal })
    }

    // ── COUNT ─────────────────────────────────────────────────────────────────

    fn parse_count(&mut self) -> Result<QueryCommand, GraphError> {
        self.expect_keyword("COUNT")?;
        match self.next() {
            Some(Token::Keyword(k)) if k == "NODES" => Ok(QueryCommand::CountNodes),
            Some(Token::Keyword(k)) if k == "EDGES" => Ok(QueryCommand::CountEdges),
            other => Err(parse_error(format!("expected NODES or EDGES after COUNT, got {other:?}"))),
        }
    }

    // ── Primitive helpers ─────────────────────────────────────────────────────

    fn parse_op(&mut self) -> Result<ComparisonOp, GraphError> {
        match self.next() {
            Some(Token::Eq)    => Ok(ComparisonOp::Eq),
            Some(Token::NotEq) => Ok(ComparisonOp::NotEq),
            Some(Token::Lt)    => Ok(ComparisonOp::Lt),
            Some(Token::LtEq)  => Ok(ComparisonOp::LtEq),
            Some(Token::Gt)    => Ok(ComparisonOp::Gt),
            Some(Token::GtEq)  => Ok(ComparisonOp::GtEq),
            other => Err(parse_error(format!("expected comparison operator, got {other:?}"))),
        }
    }

    fn parse_value_lit(&mut self) -> Result<Value, GraphError> {
        match self.next() {
            Some(Token::StringLit(s)) => Ok(Value::Text(s)),
            Some(Token::IntLit(i))    => Ok(Value::Integer(i)),
            Some(Token::FloatLit(f))  => Ok(Value::Float(f)),
            Some(Token::BoolLit(b))   => Ok(Value::Boolean(b)),
            other => Err(parse_error(format!("expected a value literal, got {other:?}"))),
        }
    }

    fn parse_node_ref(&mut self) -> Result<NodeId, GraphError> {
        match self.next() {
            Some(Token::NodeRef(id)) => Ok(NodeId(id)),
            other => Err(parse_error(format!(
                "expected a node reference (e.g. N0), got {other:?}"
            ))),
        }
    }

    fn consume_ident(&mut self) -> Result<String, GraphError> {
        match self.next() {
            Some(Token::Ident(s)) => Ok(s),
            other => Err(parse_error(format!("expected identifier, got {other:?}"))),
        }
    }

    /// Accept an identifier or any single-word token (for variable names in patterns).
    fn consume_ident_or_any(&mut self) -> Result<String, GraphError> {
        match self.next() {
            Some(Token::Ident(s)) => Ok(s),
            Some(_) => Ok("_".to_string()),
            None => Err(parse_error("expected variable name".into())),
        }
    }

    fn expect_token(&mut self, expected: Token) -> Result<(), GraphError> {
        match self.next() {
            Some(tok) if tok == expected => Ok(()),
            Some(other) => Err(parse_error(format!("expected {expected:?}, got {other:?}"))),
            None => Err(parse_error(format!("expected {expected:?} but reached end of input"))),
        }
    }

    fn expect_dash(&mut self) -> Result<(), GraphError> {
        match self.next() {
            Some(Token::Dash) => Ok(()),
            other => Err(parse_error(format!("expected '-', got {other:?}"))),
        }
    }
}

fn lex_error(msg: String) -> GraphError {
    GraphError::DeserializationError(format!("[CypherLite lexer] {msg}"))
}

fn parse_error(msg: String) -> GraphError {
    GraphError::DeserializationError(format!("[CypherLite] {msg}"))
}

// ── Re-export for easy construction ──────────────────────────────────────────

impl CypherLiteLanguage {
    pub fn parse_only(input: &str) -> Result<QueryCommand, GraphError> {
        Parser::new_result(input)?.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{ComparisonOp, QueryCommand, TraversalKind};

    fn parse(q: &str) -> QueryCommand {
        CypherLiteLanguage::parse_only(q)
            .unwrap_or_else(|e| panic!("Parse failed for «{q}»: {e}"))
    }

    fn parse_err(q: &str) -> String {
        CypherLiteLanguage::parse_only(q).unwrap_err().to_string()
    }

    #[test]
    fn match_all_nodes() {
        assert!(matches!(parse("MATCH (n) RETURN n"), QueryCommand::MatchNodes(_)));
    }

    #[test]
    fn match_nodes_with_label() {
        if let QueryCommand::MatchNodes(f) = parse("MATCH (n:City) RETURN n") {
            assert_eq!(f.label.as_deref(), Some("City"));
        } else { panic!(); }
    }

    #[test]
    fn match_nodes_where_property_gt() {
        if let QueryCommand::MatchNodes(f) =
            parse("MATCH (n:City) WHERE n.population > 1000000 RETURN n")
        {
            assert_eq!(f.property_conditions[0].key, "population");
            assert_eq!(f.property_conditions[0].op, ComparisonOp::Gt);
        } else { panic!(); }
    }

    #[test]
    fn match_all_relationships() {
        assert!(matches!(parse("MATCH ()-[r]->() RETURN r"), QueryCommand::MatchEdges(_)));
    }

    #[test]
    fn match_relationships_with_label() {
        if let QueryCommand::MatchEdges(f) = parse("MATCH ()-[r:RAIL]->() RETURN r") {
            assert_eq!(f.label.as_deref(), Some("RAIL"));
        } else { panic!(); }
    }

    #[test]
    fn match_edges_where_weight_lt() {
        if let QueryCommand::MatchEdges(f) =
            parse("MATCH ()-[r]->() WHERE r.weight < 500 RETURN r")
        {
            let wc = f.weight_condition.unwrap();
            assert_eq!(wc.op, ComparisonOp::Lt);
            assert!((wc.value - 500.0).abs() < f64::EPSILON);
        } else { panic!(); }
    }

    #[test]
    fn traverse_bfs() {
        if let QueryCommand::Traverse { kind, start } = parse("TRAVERSE BFS FROM N3") {
            assert_eq!(kind, TraversalKind::Bfs);
            assert_eq!(start, NodeId(3));
        } else { panic!(); }
    }

    #[test]
    fn traverse_dfs() {
        if let QueryCommand::Traverse { kind, .. } = parse("TRAVERSE DFS FROM N0") {
            assert_eq!(kind, TraversalKind::Dfs);
        } else { panic!(); }
    }

    #[test]
    fn path_from_to() {
        if let QueryCommand::ShortestPath { start, goal } = parse("PATH FROM N1 TO N4") {
            assert_eq!(start, NodeId(1));
            assert_eq!(goal, NodeId(4));
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
    fn lex_error_on_unknown_char() {
        // '@' is not part of the grammar
        assert!(parse_err("MATCH (@) RETURN n").contains("CypherLite"));
    }

    #[test]
    fn node_ref_lexed_correctly() {
        if let QueryCommand::Traverse { start, .. } = parse("TRAVERSE BFS FROM N42") {
            assert_eq!(start, NodeId(42));
        } else { panic!(); }
    }
}
