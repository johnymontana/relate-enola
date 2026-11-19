//! The request envelope: the one JSON line the Relate host writes to the
//! child's stdin before the MCP session starts, keyed by the manifest's flag
//! names and its positional name (verbatim: `SOURCE`).

use serde_json::{Map, Value};
use std::fmt;

/// The positional's key, exactly as `package/extension.json` declares it.
pub const SOURCE_KEY: &str = "SOURCE";

/// The resolved invocation, as the host parsed it from argv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Envelope {
    /// `SOURCE`, absent when the user omitted it (meaning the working directory).
    pub source: Option<String>,
    /// `--snapshot`: import an existing snapshot directory without running Enola.
    pub snapshot: bool,
    /// `--source-name`: overrides the canonical-path source identity. Empty means unset.
    pub source_name: String,
    /// `--keep-snapshots`: skip pruning older snapshots of the same source.
    pub keep_snapshots: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeError(String);

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the request envelope is invalid: {}", self.0)
    }
}

impl std::error::Error for EnvelopeError {}

impl Envelope {
    /// Decode the envelope object. Unknown keys are ignored (a newer host may
    /// add some); a known key with the wrong JSON type is an error, since the
    /// host's kind-checked parse should have made that impossible.
    pub fn from_map(map: &Map<String, Value>) -> Result<Self, EnvelopeError> {
        Ok(Self {
            source: optional_string(map, SOURCE_KEY)?,
            snapshot: bool_flag(map, "snapshot")?,
            source_name: optional_string(map, "source-name")?.unwrap_or_default(),
            keep_snapshots: bool_flag(map, "keep-snapshots")?,
        })
    }
}

fn optional_string(map: &Map<String, Value>, key: &str) -> Result<Option<String>, EnvelopeError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(EnvelopeError(format!(
            "'{key}' must be a string, got {other}"
        ))),
    }
}

fn bool_flag(map: &Map<String, Value>, key: &str) -> Result<bool, EnvelopeError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(EnvelopeError(format!(
            "'{key}' must be a boolean, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn decodes_the_shape_relate_sends() {
        // Observed from Relate 1.1.0 in the Phase 0 spike.
        let e = Envelope::from_map(&map(json!({"SOURCE": ".", "snapshot": false}))).unwrap();
        assert_eq!(e.source.as_deref(), Some("."));
        assert!(!e.snapshot && !e.keep_snapshots && e.source_name.is_empty());
    }

    #[test]
    fn source_is_absent_when_omitted() {
        let e = Envelope::from_map(&map(
            json!({"snapshot": true, "source-name": "api", "keep-snapshots": true}),
        ))
        .unwrap();
        assert_eq!(e.source, None);
        assert!(e.snapshot && e.keep_snapshots);
        assert_eq!(e.source_name, "api");
    }

    #[test]
    fn lowercase_source_is_not_the_positional() {
        let e = Envelope::from_map(&map(json!({"source": "elsewhere"}))).unwrap();
        assert_eq!(e.source, None);
    }

    #[test]
    fn wrong_types_are_refused() {
        assert!(Envelope::from_map(&map(json!({"snapshot": "yes"}))).is_err());
        assert!(Envelope::from_map(&map(json!({"SOURCE": 7}))).is_err());
    }
}
