//! Phase 0 packaging spike: embed enola, speak the Relate mount's MCP wire,
//! materialize enola to a content-addressed cache dir, run it, report.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

static ENOLA: &[u8] = include_bytes!(env!("ENOLA_BIN"));

struct Wire { out: std::io::Stdout, inp: std::io::StdinLock<'static>, id: u64 }

impl Wire {
    fn line(&mut self) -> Option<Value> {
        let mut s = String::new();
        match self.inp.read_line(&mut s) { Ok(0) | Err(_) => None, Ok(_) => serde_json::from_str(&s).ok() }
    }
    fn send(&mut self, v: &Value) { let mut o = self.out.lock(); writeln!(o, "{v}").unwrap(); o.flush().unwrap(); }
    fn request(&mut self, method: &str, params: Value) -> Option<Value> {
        self.id += 1;
        let id = self.id;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let v = self.line()?;
            if v.get("id").and_then(Value::as_u64) == Some(id) { return Some(v); }
        }
    }
    fn tool(&mut self, name: &str, args: Value) -> Result<Option<Value>, String> {
        match self.request("tools/call", json!({"name":name,"arguments":args})) {
            None => Ok(None),
            Some(v) if v.get("error").is_some() => Err(format!("{name} refused: {}", v["error"])),
            Some(v) => Ok(Some(v)),
        }
    }
}

fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") { return home.join("Library/Caches/relate-enola"); }
    match std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p.join("relate-enola"),
        _ => home.join(".cache/relate-enola"),
    }
}

fn materialize() -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let sha = format!("{:x}", Sha256::digest(ENOLA));
    let dir = cache_dir().join("enola").join(&sha);
    let bin = dir.join("enola");
    if let Ok(existing) = std::fs::read(&bin) {
        if format!("{:x}", Sha256::digest(&existing)) == sha { return Ok(bin); }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".enola.{}.tmp", std::process::id()));
    std::fs::write(&tmp, ENOLA).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &bin).map_err(|e| format!("rename: {e}"))?;
    Ok(bin)
}

fn run() -> Result<(), String> {
    let stdin: &'static std::io::Stdin = Box::leak(Box::new(std::io::stdin()));
    let mut w = Wire { out: std::io::stdout(), inp: stdin.lock(), id: 0 };
    let envelope = w.line().ok_or("no request envelope")?;
    let init = w.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"enola-spike","version":env!("CARGO_PKG_VERSION")}})).ok_or("host closed before initialize")?;
    if init.get("error").is_some() { return Err(format!("initialize refused: {}", init["error"])); }
    w.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    // Probe: under `extension check` the session ends here.
    let probe = w.tool("read_cypher", json!({"statement":"MATCH (n) RETURN count(n) AS n","parameters":{}}))?;
    let Some(probe) = probe else { return Ok(()) };
    let t0 = std::time::Instant::now();
    let bin = materialize()?;
    let materialize_ms = t0.elapsed().as_millis();
    let out = Command::new(&bin).arg("--version").stdin(Stdio::null())
        .env("ENOLA_NO_UPDATE_CHECK", "1").env("ENOLA_NO_PROMPTS", "1").output();
    let out = match out {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && bin.exists() => return Err(format!(
            "enola could not start: {} exists but the system lacks its glibc loader (Alpine: `apk add gcompat`); use --snapshot instead", bin.display())),
        Err(e) => return Err(format!("enola could not start: {e}")),
    };
    if !out.status.success() { return Err(format!("enola exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr))); }
    let text = format!("spike ok: {} | embedded {} bytes | cached at {} ({} ms) | envelope {} | probe {}",
        String::from_utf8_lossy(&out.stdout).trim(), ENOLA.len(), bin.display(), materialize_ms, envelope,
        probe["result"]["content"][0]["text"].as_str().unwrap_or("?"));
    w.tool("report_output", json!({"text": text, "starters": ["MATCH (n) RETURN count(n)"]}))?;
    Ok(())
}

fn main() {
    if let Err(e) = run() { eprintln!("enola-spike: {e}"); std::process::exit(1); }
}
