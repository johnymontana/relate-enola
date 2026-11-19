//! The wire client against an in-process fake host over real pipes.

use super::*;
use serde_json::json;
use std::io::{BufReader, PipeReader, PipeWriter};
use std::thread::JoinHandle;

/// What the fake host does with one request.
enum Answer {
    Result(Value),
    Tool(Value),
    ToolError(i64, &'static str),
    IsError(&'static str),
    /// Close the session instead of answering (how `extension check` ends).
    Close,
}

/// A fake Relate host: writes `envelope`, then answers each request with
/// `respond`. Returns every message the child sent, once the child drops its
/// end of the pipes or the host closes.
struct FakeHost {
    handle: JoinHandle<Vec<Value>>,
}

impl FakeHost {
    fn spawn<F>(envelope: &str, mut respond: F) -> (Client<BufReader<PipeReader>, PipeWriter>, Self)
    where
        F: FnMut(&Value) -> Answer + Send + 'static,
    {
        let (child_in, mut host_out) = std::io::pipe().unwrap();
        let (host_in, child_out) = std::io::pipe().unwrap();
        writeln!(host_out, "{envelope}").unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut reader = BufReader::new(host_in);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return seen;
                }
                let msg: Value = serde_json::from_str(&line).unwrap();
                seen.push(msg.clone());
                let Some(id) = msg.get("id").cloned() else {
                    continue;
                };
                let reply = match respond(&msg) {
                    Answer::Result(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                    Answer::Tool(v) => json!({"jsonrpc": "2.0", "id": id, "result": {
                        "content": [{"type": "text", "text": v.to_string()}], "isError": false}}),
                    Answer::ToolError(code, m) => json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": code, "message": m}}),
                    Answer::IsError(m) => json!({"jsonrpc": "2.0", "id": id, "result": {
                        "content": [{"type": "text", "text": m}], "isError": true}}),
                    Answer::Close => return seen,
                };
                // Interleave noise the client must skip: a notification and a stray id.
                writeln!(
                    host_out,
                    r#"{{"jsonrpc":"2.0","method":"notifications/message"}}"#
                )
                .unwrap();
                writeln!(host_out, r#"{{"jsonrpc":"2.0","id":999999,"result":{{}}}}"#).unwrap();
                writeln!(host_out, "{reply}").unwrap();
            }
        });
        (
            Client::new(BufReader::new(child_in), child_out),
            Self { handle },
        )
    }

    fn finish(self, client: Client<BufReader<PipeReader>, PipeWriter>) -> Vec<Value> {
        drop(client);
        self.handle.join().unwrap()
    }
}

fn init_result(caps: Value) -> Answer {
    Answer::Result(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "relate", "version": "1.1.0"},
        "commitPolicy": "per-batch",
        "caps": caps,
    }))
}

/// A host that completes the handshake with `caps` and then answers tools with `tool`.
fn host<F>(caps: Value, mut tool: F) -> impl FnMut(&Value) -> Answer + Send + 'static
where
    F: FnMut(&Value) -> Answer + Send + 'static,
{
    move |msg| match msg["method"].as_str() {
        Some("initialize") => init_result(caps.clone()),
        Some("tools/call") => tool(msg),
        other => panic!("unexpected request {other:?}"),
    }
}

fn tool_calls(seen: &[Value]) -> Vec<&Value> {
    seen.iter()
        .filter(|m| m["method"] == "tools/call")
        .collect()
}

#[test]
fn handshake_order_and_caps() {
    let caps = json!({"maxStatementBytes": 1234, "maxRowsPerStatement": 7, "maxStatements": 9, "maxRequestBytes": 99999});
    let (mut c, h) = FakeHost::spawn(
        r#"{"SOURCE":".","snapshot":false}"#,
        host(caps, |_| Answer::Tool(json!({}))),
    );
    let env = c.read_envelope().unwrap();
    assert_eq!(env["SOURCE"], ".");
    let got = c.initialize("enola-to-graph", "0.1.0").unwrap().clone();
    assert_eq!(
        got,
        Caps {
            max_statement_bytes: 1234,
            max_rows_per_statement: 7,
            max_statements: 9,
            max_request_bytes: Some(99999)
        }
    );
    c.report_output("done", &[]).unwrap();
    let seen = h.finish(c);
    assert_eq!(seen[0]["method"], "initialize");
    assert_eq!(seen[0]["id"], 1);
    assert_eq!(seen[0]["params"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(seen[0]["params"]["clientInfo"]["name"], "enola-to-graph");
    assert_eq!(
        seen[1],
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    );
    assert!(
        seen[1].get("id").is_none(),
        "the confirmation is a notification"
    );
    assert_eq!(seen[2]["params"]["name"], "report_output");
}

#[test]
fn missing_caps_fall_back_to_the_spec_defaults() {
    let (mut c, h) = FakeHost::spawn("{}", host(json!({}), |_| Answer::Tool(json!({}))));
    c.read_envelope().unwrap();
    assert_eq!(*c.initialize("x", "0").unwrap(), Caps::default());
    h.finish(c);
}

#[test]
fn a_different_protocol_revision_is_refused() {
    let (mut c, h) = FakeHost::spawn("{}", |_| {
        Answer::Result(json!({"protocolVersion": "2099-01-01"}))
    });
    c.read_envelope().unwrap();
    let err = c.initialize("x", "0").unwrap_err();
    assert!(
        matches!(err, WireError::Protocol(ref m) if m.contains("2099-01-01")),
        "{err}"
    );
    h.finish(c);
}

#[test]
fn no_envelope_is_a_protocol_error() {
    let (child_in, host_out) = std::io::pipe().unwrap();
    drop(host_out);
    let mut c = Client::new(BufReader::new(child_in), Vec::new());
    assert!(matches!(c.read_envelope(), Err(WireError::Protocol(_))));
}

#[test]
fn read_cypher_decodes_rows_and_skips_noise() {
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({}), |m| {
            assert_eq!(m["params"]["name"], "read_cypher");
            assert_eq!(m["params"]["arguments"]["parameters"], json!({"k": "v"}));
            Answer::Tool(json!({"columns": ["n"], "rows": [{"n": 0}]}))
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    let r = c
        .read_cypher("MATCH (n) RETURN count(n) AS n", json!({"k": "v"}))
        .unwrap();
    assert_eq!(
        r,
        ReadResult {
            columns: vec!["n".into()],
            rows: vec![json!({"n": 0})]
        }
    );
    h.finish(c);
}

#[test]
fn session_closed_at_the_probe_is_reported_as_closed() {
    // `relate extension check` serves the tool table and ends the session at
    // the child's first tools/call: the child must see Closed, not an error.
    let (mut c, h) = FakeHost::spawn("{}", host(json!({}), |_| Answer::Close));
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    let err = c
        .read_cypher("MATCH (s:EnolaSnapshot) RETURN count(s)", json!({}))
        .unwrap_err();
    assert!(matches!(err, WireError::Closed), "{err}");
    h.finish(c);
}

#[test]
fn refusals_are_typed() {
    let mut n = 0;
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({}), move |_| {
            n += 1;
            if n == 1 {
                Answer::ToolError(-32602, "Only MATCH queries are allowed for read-query")
            } else {
                Answer::IsError("boom")
            }
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    match c.read_cypher("CREATE (n)", json!({})).unwrap_err() {
        WireError::Refused {
            tool,
            code,
            message,
        } => {
            assert_eq!((tool.as_str(), code), ("read_cypher", Some(-32602)));
            assert!(message.contains("Only MATCH"));
        }
        other => panic!("{other}"),
    }
    match c.write_cypher("X", json!({})).unwrap_err() {
        WireError::Refused {
            code: None,
            message,
            ..
        } => assert_eq!(message, "boom"),
        other => panic!("{other}"),
    }
    h.finish(c);
}

#[test]
fn write_rows_chunks_at_the_row_cap_and_carries_extra_params() {
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({"maxRowsPerStatement": 3}), |_| {
            Answer::Tool(json!({"counts": {}}))
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    let mut extra = Map::new();
    extra.insert("snapshot_key".into(), json!("v1/abc/sha256:00"));
    let calls = c
        .write_rows(
            "UNWIND $rows AS row MERGE (n:N {id: row.id})",
            &extra,
            (0..7).map(|i| json!({"id": i})),
        )
        .unwrap();
    assert_eq!(calls, 3);
    let seen = h.finish(c);
    let calls = tool_calls(&seen);
    let sizes: Vec<usize> = calls
        .iter()
        .map(|m| {
            m["params"]["arguments"]["parameters"]["rows"]
                .as_array()
                .unwrap()
                .len()
        })
        .collect();
    assert_eq!(sizes, [3, 3, 1]);
    for m in &calls {
        assert_eq!(m["params"]["name"], "write_cypher");
        assert_eq!(
            m["params"]["arguments"]["parameters"]["snapshot_key"],
            "v1/abc/sha256:00"
        );
    }
    let ids: Vec<i64> = calls
        .iter()
        .flat_map(|m| {
            m["params"]["arguments"]["parameters"]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_i64().unwrap())
        })
        .collect();
    assert_eq!(
        ids,
        (0..7).collect::<Vec<_>>(),
        "every row lands once, in order"
    );
}

#[test]
fn write_rows_chunks_at_the_byte_cap() {
    // Each row {"id":"xxxxxxxxxx"} is 19 bytes; a 41-byte cap fits exactly
    // two (2 framing + 19 + 1 comma + 19 = 41), never three.
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({"maxStatementBytes": 41}), |_| {
            Answer::Tool(json!({}))
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    let rows = (0..5).map(|_| json!({"id": "xxxxxxxxxx"}));
    assert_eq!(c.write_rows("X", &Map::new(), rows).unwrap(), 3);
    let seen = h.finish(c);
    for m in tool_calls(&seen) {
        let arr = &m["params"]["arguments"]["parameters"]["rows"];
        assert!(arr.to_string().len() <= 41, "{arr}");
    }
}

#[test]
fn an_oversized_row_is_refused_before_anything_ships() {
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({"maxStatementBytes": 40}), |_| {
            Answer::Tool(json!({}))
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    let rows = vec![json!({"id": 1}), json!({"blob": "y".repeat(100)})];
    assert!(matches!(
        c.write_rows("X", &Map::new(), rows),
        Err(WireError::RowTooLarge { limit: 40, .. })
    ));
    let seen = h.finish(c);
    assert!(
        tool_calls(&seen).is_empty(),
        "the small chunk never left the child"
    );
}

#[test]
fn an_empty_phase_makes_no_call() {
    let (mut c, h) = FakeHost::spawn("{}", host(json!({}), |_| Answer::Tool(json!({}))));
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    assert_eq!(
        c.write_rows("X", &Map::new(), std::iter::empty()).unwrap(),
        0
    );
    assert!(tool_calls(&h.finish(c)).is_empty());
}

#[test]
fn update_schema_returns_created_names() {
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({}), |m| {
            assert_eq!(m["params"]["name"], "update_schema");
            Answer::Tool(json!({"created": ["enola_fact_key"]}))
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    assert_eq!(
        c.update_schema("CREATE CONSTRAINT ...").unwrap(),
        ["enola_fact_key"]
    );
    h.finish(c);
}

#[test]
fn batch_cypher_respects_the_statement_cap() {
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({"maxStatements": 2}), |_| {
            Answer::Tool(json!({"results": []}))
        }),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    let three: Vec<(String, Value)> = (0..3).map(|i| (format!("S{i}"), json!({}))).collect();
    assert!(matches!(
        c.batch_cypher(&three),
        Err(WireError::Protocol(_))
    ));
    c.batch_cypher(&three[..2]).unwrap();
    assert_eq!(tool_calls(&h.finish(c)).len(), 1);
}

#[test]
fn requests_over_the_request_cap_are_not_sent() {
    let (mut c, h) = FakeHost::spawn(
        "{}",
        host(json!({"maxRequestBytes": 200}), |_| Answer::Tool(json!({}))),
    );
    c.read_envelope().unwrap();
    c.initialize("x", "0").unwrap();
    assert!(matches!(
        c.write_cypher(&"z".repeat(500), json!({})),
        Err(WireError::Protocol(_))
    ));
    assert!(tool_calls(&h.finish(c)).is_empty());
}
