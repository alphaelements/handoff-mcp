//! Content hashing for memory deduplication and re-injection tracking.
//!
//! Uses FNV-1a (64-bit) — small, fast, std-only, no dependency. The hash is of
//! the *canonical* token form (`tokenize().join(" ")`), so edits that only
//! change whitespace or letter case produce the **same** hash and are therefore
//! not treated as a content change.

use crate::tokenize::tokenize;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a hash of raw bytes, returned as a lowercase hex string.
pub fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// Stable content hash of `text`: hash of its canonical token form. Whitespace-
/// and case-only differences collapse to the same hash.
///
/// Note: the canonical form deduplicates word tokens but preserves order, and
/// also incorporates the cross-language n-grams, so two texts with the same
/// words in a different order differ — intentional, since memory text order
/// carries meaning.
pub fn content_hash(text: &str) -> String {
    let canonical = tokenize(text).join(" ");
    fnv1a_hex(canonical.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_across_runs() {
        assert_eq!(content_hash("hello world"), content_hash("hello world"));
    }

    #[test]
    fn whitespace_only_change_same_hash() {
        assert_eq!(content_hash("hello   world"), content_hash("hello world"),);
    }

    #[test]
    fn case_only_change_same_hash() {
        assert_eq!(content_hash("Hello World"), content_hash("hello world"));
    }

    #[test]
    fn different_content_different_hash() {
        assert_ne!(content_hash("hello world"), content_hash("goodbye world"));
    }

    #[test]
    fn japanese_stable() {
        assert_eq!(content_hash("メモリ機能"), content_hash("メモリ機能"));
        assert_ne!(content_hash("メモリ機能"), content_hash("セッション"));
    }

    #[test]
    fn empty_is_stable() {
        assert_eq!(content_hash(""), content_hash(""));
    }

    #[test]
    fn hex_format_is_16_chars() {
        assert_eq!(fnv1a_hex(b"anything").len(), 16);
    }
}
