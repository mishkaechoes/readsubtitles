//! On-disk cache of synthesised audio.
//!
//! Remote synthesis is billed per character, so a line that has been generated
//! once should never be paid for twice. Films get re-watched and stock phrases
//! recur, and a cache hit also removes the network round trip entirely, which
//! makes the repeat faster as well as free.

use std::io;
use std::path::{Path, PathBuf};

/// FNV-1a over the inputs that determine the audio.
///
/// Not cryptographic, and it does not need to be: a collision costs the wrong
/// cached line, and the inputs are short.
fn key(text: &str, voice: &str, model: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [text, "\u{0}", voice, "\u{0}", model] {
        for b in part.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

pub struct AudioCache {
    dir: PathBuf,
}

impl AudioCache {
    /// Cache under the user's caches directory, falling back to a temp dir.
    #[must_use]
    pub fn new(subdir: &str) -> Self {
        let base = std::env::var_os("HOME")
            .map(|h| Path::new(&h).join("Library/Caches"))
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(subdir);
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn path(&self, text: &str, voice: &str, model: &str) -> PathBuf {
        self.dir.join(format!("{}.pcm", key(text, voice, model)))
    }

    pub fn get(&self, text: &str, voice: &str, model: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(text, voice, model)).ok()
    }

    pub fn put(&self, text: &str, voice: &str, model: &str, pcm: &[u8]) -> io::Result<()> {
        std::fs::write(self.path(text, voice, model), pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_inputs_give_the_same_key() {
        assert_eq!(key("hello", "v1", "m1"), key("hello", "v1", "m1"));
    }

    #[test]
    fn any_input_changing_changes_the_key() {
        let base = key("hello", "v1", "m1");
        assert_ne!(base, key("hello!", "v1", "m1"));
        assert_ne!(base, key("hello", "v2", "m1"));
        assert_ne!(base, key("hello", "v1", "m2"));
    }

    #[test]
    fn field_boundaries_are_not_ambiguous() {
        // Without a separator these two would hash identically.
        assert_ne!(key("ab", "c", "d"), key("a", "bc", "d"));
    }

    #[test]
    fn round_trips_through_disk() {
        let c = AudioCache::new("readsubtitles-test-cache");
        c.put("round trip", "v", "m", &[1, 2, 3, 4]).unwrap();
        assert_eq!(
            c.get("round trip", "v", "m").as_deref(),
            Some(&[1, 2, 3, 4][..])
        );
        assert!(c.get("never stored", "v", "m").is_none());
    }
}
