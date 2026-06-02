// GraphServer — a single-threaded TCP server that exposes the database over
// a simple line-based text protocol.
//
// ── Protocol ─────────────────────────────────────────────────────────────────
//
//   Client → Server (one line per request):
//     [lang:]<query text>\n
//
//     lang is optional.  Accepted values: "simple" (default), "cypher".
//     Examples:
//       MATCH NODE WHERE label = "City"\n
//       simple:MATCH NODE WHERE label = "City"\n
//       cypher:MATCH (n:City) RETURN n\n
//       :quit\n   ← built-in server command (closes this connection)
//
//   Server → Client (multi-line response, always terminated by "---END---"):
//     OK\n
//     <result text>\n
//     ---END---\n
//
//     OR on error:
//     ERR\n
//     <error message>\n
//     ---END---\n
//
// ── Design notes ─────────────────────────────────────────────────────────────
//
//   Connections are handled sequentially in a single thread.  This keeps the
//   code simple and avoids the need to make LayeredGraphDatabase Send + Sync.
//
//   For concurrent access, the next step would be:
//     1. Arc<Mutex<LayeredGraphDatabase>> shared across threads, OR
//     2. MVCC (multi-version concurrency control) — see doc 10.
//
//   Each connection maintains its own "current language" state so users can
//   switch without re-specifying the language on every query.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};

use crate::database::layered::LayeredGraphDatabase;
use crate::query::languages::{
    cypher_lite::CypherLiteLanguage,
    simple::SimpleQueryLanguage,
};

// ── Public server struct ──────────────────────────────────────────────────────

pub struct GraphServer {
    db:   LayeredGraphDatabase,
    addr: String,
}

impl GraphServer {
    pub fn new(db: LayeredGraphDatabase, addr: impl Into<String>) -> Self {
        Self { db, addr: addr.into() }
    }

    /// Bind the TCP socket and block serving connections one at a time.
    ///
    /// This is intentionally single-threaded for educational clarity.
    /// Every connection is fully handled before the next one is accepted.
    pub fn start(&mut self) -> std::io::Result<()> {
        let listener = TcpListener::bind(&self.addr)?;
        println!("[server] Listening on {}  (protocol: line-based text)", self.addr);
        println!("[server] Send  '<query>\\n' or 'simple:<query>\\n' or 'cypher:<query>\\n'");
        println!("[server] Send  ':quit\\n' to close the connection");
        println!("[server] Press Ctrl-C to stop the server\n");

        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let peer = stream.peer_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_else(|_| "unknown".into());
                    println!("[server] Connection from {peer}");
                    handle_connection(stream, &mut self.db);
                    println!("[server] Connection from {peer} closed");
                }
                Err(e) => eprintln!("[server] Accept error: {e}"),
            }
        }

        Ok(())
    }

    /// Return the address this server is (or will be) bound to.
    pub fn addr(&self) -> &str { &self.addr }
}

// ── Connection handler ────────────────────────────────────────────────────────

/// Selected query language for a connection session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lang { Simple, Cypher }

impl Lang {
    fn name(self) -> &'static str {
        match self { Lang::Simple => "simple", Lang::Cypher => "cypher" }
    }
}

impl Default for Lang {
    fn default() -> Self { Lang::Simple }
}

fn handle_connection(stream: TcpStream, db: &mut LayeredGraphDatabase) {
    // Clone the stream so we can have both a reader and a writer.
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => { eprintln!("[server] Failed to clone stream: {e}"); return; }
    };

    let reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);
    let mut current_lang = Lang::default();

    // Send welcome banner.
    let _ = writeln!(writer,
        "# AdGraphDb  |  language: {}  |  :quit to disconnect  |  :lang simple|cypher",
        current_lang.name()
    );
    let _ = writeln!(writer, "---END---");
    let _ = writer.flush();

    for raw_line in reader.lines() {
        let line = match raw_line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };

        if line.is_empty() { continue; }

        // ── Built-in meta-commands ────────────────────────────────────────
        if line.eq_ignore_ascii_case(":quit") || line.eq_ignore_ascii_case(":exit") {
            let _ = writeln!(writer, "OK\nBye!");
            let _ = writeln!(writer, "---END---");
            let _ = writer.flush();
            break;
        }

        if let Some(new_lang) = parse_lang_switch(&line) {
            current_lang = new_lang;
            let _ = writeln!(writer, "OK\nLanguage switched to {}", current_lang.name());
            let _ = writeln!(writer, "---END---");
            let _ = writer.flush();
            continue;
        }

        // ── Parse language prefix ─────────────────────────────────────────
        let (lang, query) = parse_lang_prefix(&line, current_lang);

        // ── Execute ───────────────────────────────────────────────────────
        let response = match run_query(db, lang, query) {
            Ok(text)  => format!("OK\n{text}"),
            Err(text) => format!("ERR\n{text}"),
        };

        let _ = writeln!(writer, "{response}");
        let _ = writeln!(writer, "---END---");
        let _ = writer.flush();
    }
}

// ── Protocol helpers ──────────────────────────────────────────────────────────

/// Parse an optional `simple:` or `cypher:` prefix.
/// Returns (effective_lang, query_str).
fn parse_lang_prefix<'a>(line: &'a str, default: Lang) -> (Lang, &'a str) {
    if let Some(rest) = line.strip_prefix("simple:").or_else(|| line.strip_prefix("simple: ")) {
        return (Lang::Simple, rest.trim());
    }
    if let Some(rest) = line.strip_prefix("cypher:").or_else(|| line.strip_prefix("cypher: ")) {
        return (Lang::Cypher, rest.trim());
    }
    (default, line)
}

/// Detect `:lang simple` or `:lang cypher` meta-commands.
fn parse_lang_switch(line: &str) -> Option<Lang> {
    let lower = line.to_lowercase();
    if lower == ":lang simple" || lower == ":use simple" { return Some(Lang::Simple); }
    if lower == ":lang cypher" || lower == ":use cypher" { return Some(Lang::Cypher); }
    None
}

fn run_query(
    db: &mut LayeredGraphDatabase,
    lang: Lang,
    query: &str,
) -> Result<String, String> {
    let result = match lang {
        Lang::Simple => db.execute_query(&SimpleQueryLanguage, query),
        Lang::Cypher => db.execute_query(&CypherLiteLanguage,  query),
    };
    match result {
        Ok(r)  => Ok(r.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_prefix_uses_default() {
        let (lang, query) = parse_lang_prefix("MATCH NODE", Lang::Simple);
        assert_eq!(lang, Lang::Simple);
        assert_eq!(query, "MATCH NODE");
    }

    #[test]
    fn parse_simple_prefix() {
        let (lang, query) = parse_lang_prefix("simple:MATCH NODE", Lang::Cypher);
        assert_eq!(lang, Lang::Simple);
        assert_eq!(query, "MATCH NODE");
    }

    #[test]
    fn parse_cypher_prefix() {
        let (lang, query) = parse_lang_prefix("cypher:MATCH (n) RETURN n", Lang::Simple);
        assert_eq!(lang, Lang::Cypher);
        assert_eq!(query, "MATCH (n) RETURN n");
    }

    #[test]
    fn lang_switch_simple() {
        assert_eq!(parse_lang_switch(":lang simple"), Some(Lang::Simple));
        assert_eq!(parse_lang_switch(":use simple"),  Some(Lang::Simple));
    }

    #[test]
    fn lang_switch_cypher() {
        assert_eq!(parse_lang_switch(":lang cypher"), Some(Lang::Cypher));
    }

    #[test]
    fn lang_switch_unknown_returns_none() {
        assert_eq!(parse_lang_switch(":lang graphql"), None);
        assert_eq!(parse_lang_switch("just a query"),  None);
    }
}
