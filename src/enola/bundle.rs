//! The embedded Enola binary (feature `bundled-enola`) and its
//! materialization into the cache: write to a temporary file in the target
//! directory, verify the SHA-256, `chmod 0700`, rename atomically. A binary
//! already in place is reused when its hash matches. Enola's `LICENSE` and
//! `NOTICE` (and ours) are written beside it.

use super::cache::Cache;
use super::{io_err, EnolaError};
use crate::hex;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// An Enola binary compiled into this executable.
#[derive(Debug, Clone, Copy)]
pub struct Bundled {
    pub bytes: &'static [u8],
    /// Lowercase hex SHA-256 of `bytes`, verified by build.rs against enola.lock.toml.
    pub sha256: &'static str,
    pub version: &'static str,
}

pub const ENOLA_LICENSE: &str = include_str!("../../third_party/enola/LICENSE");
pub const ENOLA_NOTICE: &str = include_str!("../../third_party/enola/NOTICE");
pub const OUR_NOTICE: &str = include_str!("../../NOTICE");

#[cfg(feature = "bundled-enola")]
static BYTES: &[u8] = include_bytes!(env!("ENOLA_BUNDLED_PATH"));

/// The bundled Enola, if this build has one.
pub fn bundled() -> Option<Bundled> {
    #[cfg(feature = "bundled-enola")]
    {
        Some(Bundled {
            bytes: BYTES,
            sha256: env!("ENOLA_BUNDLED_SHA256"),
            version: env!("ENOLA_BUNDLED_VERSION"),
        })
    }
    #[cfg(not(feature = "bundled-enola"))]
    {
        None
    }
}

/// Make `b` available as an executable file in the cache; return its path.
pub fn materialize(b: &Bundled, cache: &Cache) -> Result<PathBuf, EnolaError> {
    let dir = cache.bin_dir(b.sha256);
    let bin = dir.join("enola");
    std::fs::create_dir_all(&dir).map_err(|e| io_err("cannot create", &dir, e))?;
    for (name, text) in [
        ("LICENSE", ENOLA_LICENSE),
        ("NOTICE", ENOLA_NOTICE),
        ("NOTICE.enola-to-graph", OUR_NOTICE),
    ] {
        let p = dir.join(name);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(text) {
            write_atomic(&dir, &p, text.as_bytes(), 0o644)?;
        }
    }
    if let Ok(existing) = std::fs::read(&bin) {
        if hex::sha256_hex(&existing) == b.sha256 {
            return Ok(bin);
        }
    }
    if hex::sha256_hex(b.bytes) != b.sha256 {
        return Err(EnolaError::Io(
            "the embedded Enola does not match its recorded SHA-256".into(),
        ));
    }
    write_atomic(&dir, &bin, b.bytes, 0o700)?;
    Ok(bin)
}

fn write_atomic(dir: &Path, dest: &Path, bytes: &[u8], mode: u32) -> Result<(), EnolaError> {
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| io_err("cannot write", &tmp, e))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io_err("cannot set permissions on", &tmp, e))?;
    std::fs::rename(&tmp, dest).map_err(|e| io_err("cannot install", dest, e))?;
    Ok(())
}
