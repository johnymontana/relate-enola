//! A small MCP client for the Relate command mount (PLAN.md D11).
//!
//! Implements the extension spec's *Transport contract*: read the one-line
//! request envelope, `initialize`, confirm with `notifications/initialized`,
//! then drive the import through `tools/call` on the five host tools. The
//! child is the MCP client; the host owns the store.
//!
//! The client is generic over its reader and writer so tests can run it
//! against an in-process fake host. In the executable they are the process's
//! stdin and stdout; stdout carries nothing but protocol lines (the crate
//! denies `print_stdout`).

use serde_json::{json, Map, Value};
use std::fmt;
use std::io::{BufRead, Write};

#[cfg(test)]
mod tests;

/// The MCP revision the Relate command mount serves.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Bytes a JSON array adds around its elements (`[` and `]`).
const ARRAY_FRAMING_BYTES: u64 = 2;

/// The host's per-call caps, republished in the `initialize` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// Bound on one statement's serialized `rows` array (and on a response line).
    pub max_statement_bytes: u64,
    /// Rows per `write_cypher` call.
    pub max_rows_per_statement: usize,
    /// Statements per `batch_cypher` call.
    pub max_statements: usize,
    /// Bound on one request line, when the host publishes it.
    pub max_request_bytes: Option<u64>,
}

impl Default for Caps {
    /// The values the spec documents, used when a host omits a field.
    fn default() -> Self {
        Self {
            max_statement_bytes: 8 * 1024 * 1024,
            max_rows_per_statement: 1000,
            max_statements: 4096,
            max_request_bytes: None,
        }
    }
}

impl Caps {
    fn from_init(result: &Value) -> Self {
        let d = Self::default();
        let caps = &result["caps"];
        let u = |k: &str| caps.get(k).and_then(Value::as_u64);
        Self {
            max_statement_bytes: u("maxStatementBytes").unwrap_or(d.max_statement_bytes),
            max_rows_per_statement: u("maxRowsPerStatement")
                .map_or(d.max_rows_per_statement, |n| n as usize),
            max_statements: u("maxStatements").map_or(d.max_statements, |n| n as usize),
            max_request_bytes: u("maxRequestBytes"),
        }
    }
}

/// What can go wrong on the wire.
#[derive(Debug)]
pub enum WireError {
    /// The host ended the session (stdin closed). After the probe call this
    /// is how `relate extension check` ends a run, so it is not always a failure.
    Closed,
    Io(std::io::Error),
    /// The host sent something the contract does not allow.
    Protocol(String),
    /// The host answered a tool call with an error (JSON-RPC error or `isError`).
    Refused {
        tool: String,
        code: Option<i64>,
        message: String,
    },
    /// One row is too large to fit any call under the statement cap.
    RowTooLarge {
        limit: u64,
        bytes: u64,
    },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "the Relate host ended the session"),
            Self::Io(e) => write!(f, "I/O error on the host session: {e}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
            Self::Refused {
                tool,
                code: Some(c),
                message,
            } => {
                write!(f, "the host refused '{tool}' ({c}): {message}")
            }
            Self::Refused {
                tool,
                code: None,
                message,
            } => {
                write!(f, "the host refused '{tool}': {message}")
            }
            Self::RowTooLarge { limit, bytes } => write!(
                f,
                "one row serializes to {bytes} bytes, over the host's {limit}-byte statement cap"
            ),
        }
    }
}

impl std::error::Error for WireError {}

impl From<std::io::Error> for WireError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// The `read_cypher` result.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadResult {
    pub columns: Vec<String>,
    /// One JSON object per row, keyed by column.
    pub rows: Vec<Value>,
}

/// The MCP client side of one mount session.
pub struct Client<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
    caps: Caps,
    calls: u64,
    fail_after: Option<u64>,
}

impl<R: BufRead, W: Write> Client<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 0,
            caps: Caps::default(),
            calls: 0,
            fail_after: None,
        }
    }

    /// Fault injection for tests: refuse every tool call after the first
    /// `n`, as if the process died mid-import. Never set in normal runs.
    pub fn fail_after(&mut self, n: Option<u64>) {
        self.fail_after = n;
    }

    /// The caps in force (defaults until [`Self::initialize`] succeeds).
    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    /// `tools/call` requests sent so far.
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Read the one-line request envelope. It precedes everything else.
    pub fn read_envelope(&mut self) -> Result<Map<String, Value>, WireError> {
        let line = self
            .read_line()?
            .ok_or_else(|| WireError::Protocol("the host wrote no request envelope".into()))?;
        match serde_json::from_str::<Value>(&line) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(other) => Err(WireError::Protocol(format!(
                "the envelope is not an object: {other}"
            ))),
            Err(e) => Err(WireError::Protocol(format!(
                "the envelope is not JSON: {e}"
            ))),
        }
    }

    /// The handshake: `initialize`, check the revision, record the caps, and
    /// confirm with the id-less `notifications/initialized`.
    pub fn initialize(&mut self, name: &str, version: &str) -> Result<&Caps, WireError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": name, "version": version},
            }),
        )?;
        let served = result.get("protocolVersion").and_then(Value::as_str);
        if served != Some(PROTOCOL_VERSION) {
            return Err(WireError::Protocol(format!(
                "the host serves MCP {served:?}, this extension speaks {PROTOCOL_VERSION}"
            )));
        }
        self.caps = Caps::from_init(&result);
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(&self.caps)
    }

    /// A read-only Cypher query (`MATCH` only; the host refuses writes).
    pub fn read_cypher(
        &mut self,
        statement: &str,
        parameters: Value,
    ) -> Result<ReadResult, WireError> {
        let v = self.call_tool(
            "read_cypher",
            json!({"statement": statement, "parameters": parameters}),
        )?;
        let columns = v
            .get("columns")
            .and_then(Value::as_array)
            .ok_or_else(|| WireError::Protocol(format!("read_cypher answered no columns: {v}")))?
            .iter()
            .map(|c| c.as_str().unwrap_or_default().to_string())
            .collect();
        let rows = v
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(ReadResult { columns, rows })
    }

    /// One `write_cypher` call; returns the host's answer (`counts`, …).
    pub fn write_cypher(&mut self, statement: &str, parameters: Value) -> Result<Value, WireError> {
        self.call_tool(
            "write_cypher",
            json!({"statement": statement, "parameters": parameters}),
        )
    }

    /// Write `rows` through `statement` as `$rows`, one `write_cypher` call per
    /// chunk. A chunk holds at most `max_rows_per_statement` rows and its
    /// serialized rows array stays within `max_statement_bytes` (the measure
    /// the host applies). `extra` parameters ride along with every chunk.
    /// Returns the number of calls made; an empty `rows` makes none.
    pub fn write_rows<I>(
        &mut self,
        statement: &str,
        extra: &Map<String, Value>,
        rows: I,
    ) -> Result<u64, WireError>
    where
        I: IntoIterator<Item = Value>,
    {
        let limit = self.caps.max_statement_bytes;
        let max_rows = self.caps.max_rows_per_statement.max(1);
        let mut chunk: Vec<Value> = Vec::new();
        let mut bytes = ARRAY_FRAMING_BYTES;
        let mut calls = 0;
        for row in rows {
            let row_bytes = serde_json::to_string(&row)
                .map(|s| s.len() as u64)
                .unwrap_or(u64::MAX);
            if ARRAY_FRAMING_BYTES + row_bytes > limit {
                return Err(WireError::RowTooLarge {
                    limit,
                    bytes: row_bytes,
                });
            }
            let comma = u64::from(!chunk.is_empty());
            if bytes + comma + row_bytes > limit {
                self.write_chunk(statement, extra, std::mem::take(&mut chunk))?;
                calls += 1;
                bytes = ARRAY_FRAMING_BYTES;
            }
            bytes += u64::from(!chunk.is_empty()) + row_bytes;
            chunk.push(row);
            if chunk.len() >= max_rows {
                self.write_chunk(statement, extra, std::mem::take(&mut chunk))?;
                calls += 1;
                bytes = ARRAY_FRAMING_BYTES;
            }
        }
        if !chunk.is_empty() {
            self.write_chunk(statement, extra, chunk)?;
            calls += 1;
        }
        Ok(calls)
    }

    fn write_chunk(
        &mut self,
        statement: &str,
        extra: &Map<String, Value>,
        chunk: Vec<Value>,
    ) -> Result<(), WireError> {
        let mut params = extra.clone();
        params.insert("rows".into(), Value::Array(chunk));
        self.write_cypher(statement, Value::Object(params))
            .map(|_| ())
    }

    /// Several statements in one transaction (one per call under `per-batch`).
    pub fn batch_cypher(&mut self, statements: &[(String, Value)]) -> Result<Value, WireError> {
        if statements.len() > self.caps.max_statements {
            return Err(WireError::Protocol(format!(
                "{} statements exceed the host's {}-statement batch cap",
                statements.len(),
                self.caps.max_statements
            )));
        }
        let list: Vec<Value> = statements
            .iter()
            .map(|(s, p)| json!({"statement": s, "parameters": p}))
            .collect();
        self.call_tool("batch_cypher", json!({"statements": list}))
    }

    /// One DDL statement (index or constraint). Returns the names it created.
    pub fn update_schema(&mut self, statement: &str) -> Result<Vec<String>, WireError> {
        let v = self.call_tool("update_schema", json!({"statement": statement}))?;
        Ok(v.get("created")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The run's summary, printed by the host, with starter queries.
    pub fn report_output(&mut self, text: &str, starters: &[String]) -> Result<(), WireError> {
        self.call_tool("report_output", json!({"text": text, "starters": starters}))
            .map(|_| ())
    }

    /// `tools/call` `name`; returns the tool's JSON result (decoded from
    /// `content[0].text`).
    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, WireError> {
        if self.fail_after.is_some_and(|n| self.calls >= n) {
            return Err(WireError::Protocol(format!(
                "injected failure after {} tool calls (ENOLA_TO_GRAPH_TEST_FAIL_AFTER)",
                self.calls
            )));
        }
        self.calls += 1;
        let response =
            self.request_raw("tools/call", json!({"name": name, "arguments": arguments}))?;
        if let Some(err) = response.get("error") {
            return Err(WireError::Refused {
                tool: name.into(),
                code: err.get("code").and_then(Value::as_i64),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
        let result = response
            .get("result")
            .ok_or_else(|| WireError::Protocol(format!("'{name}' answered no result")))?;
        let text = result["content"][0]["text"].as_str().ok_or_else(|| {
            WireError::Protocol(format!("'{name}' answered no text content: {result}"))
        })?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(WireError::Refused {
                tool: name.into(),
                code: None,
                message: text.to_string(),
            });
        }
        Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
    }

    /// A request whose JSON-RPC error is a protocol failure (the handshake).
    fn request(&mut self, method: &str, params: Value) -> Result<Value, WireError> {
        let response = self.request_raw(method, params)?;
        if let Some(err) = response.get("error") {
            return Err(WireError::Protocol(format!("'{method}' refused: {err}")));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| WireError::Protocol(format!("'{method}' answered no result")))
    }

    /// Send a request and wait for the response carrying its id. Lines for
    /// other ids, and notifications, are skipped.
    fn request_raw(&mut self, method: &str, params: Value) -> Result<Value, WireError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let Some(line) = self.read_line()? else {
                return Err(WireError::Closed);
            };
            let msg: Value = serde_json::from_str(&line)
                .map_err(|e| WireError::Protocol(format!("the host wrote a non-JSON line: {e}")))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(msg);
            }
        }
    }

    fn send(&mut self, msg: &Value) -> Result<(), WireError> {
        let line = serde_json::to_string(msg).map_err(|e| WireError::Protocol(e.to_string()))?;
        if let Some(cap) = self.caps.max_request_bytes {
            if line.len() as u64 > cap {
                return Err(WireError::Protocol(format!(
                    "a {}-byte request exceeds the host's {cap}-byte request cap",
                    line.len()
                )));
            }
        }
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    /// The next non-empty line, or `None` at end of input.
    fn read_line(&mut self) -> Result<Option<String>, WireError> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_string()));
            }
        }
    }
}
