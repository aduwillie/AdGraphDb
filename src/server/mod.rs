// GraphServer — multi-threaded TCP server that exposes the database over
// a simple line-based text protocol.
//
// ── Concurrency model ─────────────────────────────────────────────────────────
//
//   The server uses SharedDatabase (Arc<Mutex<LayeredGraphDatabase>>).
//   Each incoming connection is handled in its own OS thread.
//   Threads serialize on the Mutex for each query — one query runs at a time.
//
//   This is correct for all workloads; throughput scales with client request
//   complexity, not with connection count.  For higher query throughput see
//   docs/17_concurrency_and_safety.md (RwLock + interior-mutability cache).
//
// ── Protocol ─────────────────────────────────────────────────────────────────
//
//   Client → Server (one line):
//     [lang:]<query>\n
//     :use simple | :use cypher   — switch language for this session
//     :quit | :exit               — close this connection
//
//   Server → Client:
//     OK\n<result text>\n---END---\n
//     ERR\n<error message>\n---END---\n
//
// ── Examples ─────────────────────────────────────────────────────────────────
//   echo 'MATCH NODE WHERE label = "City"' | nc localhost 7474
//   echo 'cypher:MATCH (n:City) RETURN n' | nc localhost 7474

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::concurrent::SharedDatabase;
use crate::query::languages::{
    cypher_lite::CypherLiteLanguage,
    simple::SimpleQueryLanguage,
};

// ── Public server struct ──────────────────────────────────────────────────────

pub struct GraphServer {
    db:   SharedDatabase,
    addr: String,
}

impl GraphServer {
    pub fn new(db: SharedDatabase, addr: impl Into<String>) -> Self {
        Self { db, addr: addr.into() }
    }

    /// Bind the TCP socket and serve connections, each in its own thread.
    pub fn start(&self) -> std::io::Result<()> {
        let listener = TcpListener::bind(&self.addr)?;
        println!("[server] Listening on {}  (multi-threaded, protocol: line-based text)", self.addr);
        println!("[server] Send '<query>\\n' or 'simple:<q>\\n' or 'cypher:<q>\\n'");
        println!("[server] Send ':quit\\n' to close a connection");
        println!("[server] Press Ctrl-C to stop\n");

        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let peer = stream.peer_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_else(|_| "unknown".into());
                    println!("[server] Connection from {peer}");

                    let db_handle = self.db.clone_handle();
                    thread::spawn(move || {
                        handle_connection(stream, db_handle);
                        println!("[server] Connection from {peer} closed");
                    });
                }
                Err(e) => eprintln!("[server] Accept error: {e}"),
            }
        }

        Ok(())
    }

    pub fn addr(&self) -> &str { &self.addr }
}

// ── Connection handler ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lang { Simple, Cypher }

impl Lang {
    fn name(self) -> &'static str {
        match self { Lang::Simple => "simple", Lang::Cypher => "cypher" }
    }
}

impl Default for Lang { fn default() -> Self { Lang::Simple } }

fn handle_connection(stream: TcpStream, db: SharedDatabase) {
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => { eprintln!("[server] clone stream: {e}"); return; }
    };

    let reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);
    let mut lang = Lang::default();

    // Welcome banner.
    let _ = writeln!(writer,
        "# AdGraphDb  lang:{}  :quit to disconnect  :use simple|cypher to switch",
        lang.name()
    );
    let _ = writeln!(writer, "---END---");
    let _ = writer.flush();

    for raw_line in reader.lines() {
        let line = match raw_line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if line.is_empty() { continue; }

        if line.eq_ignore_ascii_case(":quit") || line.eq_ignore_ascii_case(":exit") {
            let _ = writeln!(writer, "OK\nBye!");
            let _ = writeln!(writer, "---END---");
            let _ = writer.flush();
            break;
        }

        if let Some(new_lang) = parse_lang_switch(&line) {
            lang = new_lang;
            let _ = writeln!(writer, "OK\nLanguage: {}", lang.name());
            let _ = writeln!(writer, "---END---");
            let _ = writer.flush();
            continue;
        }

        let (effective_lang, query) = parse_lang_prefix(&line, lang);

        let response = match run_query(&db, effective_lang, query) {
            Ok(text)  => format!("OK\n{text}"),
            Err(text) => format!("ERR\n{text}"),
        };

        let _ = writeln!(writer, "{response}");
        let _ = writeln!(writer, "---END---");
        let _ = writer.flush();
    }
}

fn parse_lang_prefix<'a>(line: &'a str, default: Lang) -> (Lang, &'a str) {
    if let Some(rest) = line.strip_prefix("simple:").or_else(|| line.strip_prefix("simple: ")) {
        return (Lang::Simple, rest.trim());
    }
    if let Some(rest) = line.strip_prefix("cypher:").or_else(|| line.strip_prefix("cypher: ")) {
        return (Lang::Cypher, rest.trim());
    }
    (default, line)
}

fn parse_lang_switch(line: &str) -> Option<Lang> {
    match line.to_lowercase().as_str() {
        ":lang simple" | ":use simple" => Some(Lang::Simple),
        ":lang cypher" | ":use cypher" => Some(Lang::Cypher),
        _ => None,
    }
}

fn run_query(db: &SharedDatabase, lang: Lang, query: &str) -> Result<String, String> {
    let result = match lang {
        Lang::Simple => db.execute_query(&SimpleQueryLanguage, query),
        Lang::Cypher => db.execute_query(&CypherLiteLanguage,  query),
    };
    match result {
        Ok(r)  => Ok(r.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

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
    fn lang_switch_detection() {
        assert_eq!(parse_lang_switch(":use simple"),  Some(Lang::Simple));
        assert_eq!(parse_lang_switch(":use cypher"),  Some(Lang::Cypher));
        assert_eq!(parse_lang_switch(":lang cypher"), Some(Lang::Cypher));
        assert_eq!(parse_lang_switch("just a query"), None);
    }
}
