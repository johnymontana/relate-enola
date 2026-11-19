//! `enola-to-graph`: a Relate command extension that projects Enola
//! architecture snapshots into a Relate graph.
//!
//! The library is host-independent: [`snapshot`] reads and validates Enola's
//! exported artifacts, [`project`] turns them into deterministic graph rows,
//! [`source`] decides source identity, and [`wire`] is the MCP client the
//! executable uses to talk to the Relate host. None of it touches a database.

pub mod envelope;
pub mod graph;
pub mod hex;
pub mod project;
pub mod snapshot;
pub mod source;
pub mod wire;
