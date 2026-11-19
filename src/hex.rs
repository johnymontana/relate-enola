//! SHA-256 helpers in the `sha256:<lowercase hex>` spelling Enola uses.

use sha2::{Digest, Sha256};

/// Lowercase hex of `bytes`.
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    encode(&Sha256::digest(bytes))
}

/// `sha256:<hex>` of `bytes`, the spelling of Enola's `output_hashes`.
pub fn sha256_tagged(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// True when `s` is exactly `len` lowercase hex digits.
pub fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// True when `s` is `sha256:` followed by 64 lowercase hex digits.
pub fn is_tagged_sha256(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| is_lower_hex(h, 64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest() {
        assert_eq!(
            sha256_tagged(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn validates_spellings() {
        assert!(is_lower_hex("0123456789abcdef", 16));
        assert!(!is_lower_hex("0123456789ABCDEF", 16));
        assert!(!is_lower_hex("abc", 4));
        assert!(is_tagged_sha256(&sha256_tagged(b"")));
        assert!(!is_tagged_sha256("md5:abc"));
    }
}
