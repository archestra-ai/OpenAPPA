//! The canonical tree digest as a manifest writes it: `sha256:<64 hex>`.
//!
//! The digest itself is computed in [`crate::tree`], which stays on `std` and
//! `sha2` so a build script can call it. This is its spelling.

use std::fmt;
use std::path::Path;

use thiserror::Error;

use crate::tree::{self, TreeDigestError};

const PREFIX: &str = "sha256:";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TreeDigestParseError {
    #[error("`{0}` is not a tree digest: a digest is `sha256:` and 64 lowercase hex characters")]
    Malformed(String),
}

/// The identity of a package tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeDigest([u8; 32]);

impl TreeDigest {
    pub fn parse(text: &str) -> Result<Self, TreeDigestParseError> {
        let malformed = || TreeDigestParseError::Malformed(text.to_owned());
        let hex = text.strip_prefix(PREFIX).ok_or_else(malformed)?;
        if hex.len() != 64 {
            return Err(malformed());
        }
        let mut bytes = [0u8; 32];
        for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
            let digit = |character: u8| match character {
                b'0'..=b'9' => Some(character - b'0'),
                b'a'..=b'f' => Some(character - b'a' + 10),
                _ => None,
            };
            let high = digit(pair[0]).ok_or_else(malformed)?;
            let low = digit(pair[1]).ok_or_else(malformed)?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    /// The identity of the tree rooted at `root`.
    pub fn of_tree(root: &Path) -> Result<Self, TreeDigestError> {
        tree::canonical_tree_digest(root).map(Self)
    }

    pub fn bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for TreeDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(PREFIX)?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED: &str = "sha256:2619322b5feb343f951e2ed55ab86948a93d9460eea0fc70d46a20fdb9f87dec";

    #[test]
    fn a_digest_round_trips_through_its_spelling() {
        assert_eq!(TreeDigest::parse(PINNED).unwrap().to_string(), PINNED);
    }

    #[test]
    fn parsing_refuses_anything_but_the_spelling() {
        let hex = &PINNED[PREFIX.len()..];
        for spelling in [
            "",
            hex,
            &PINNED.to_uppercase(),
            &format!("sha512:{hex}"),
            &format!("{PREFIX}{}", &hex[..63]),
            &format!("{PREFIX}{hex}0"),
            &format!("{PREFIX}{}zz", &hex[..62]),
        ] {
            assert!(
                matches!(TreeDigest::parse(spelling), Err(TreeDigestParseError::Malformed(_))),
                "accepted {spelling}"
            );
        }
    }

    #[test]
    fn a_tree_digest_matches_the_pinned_encoding() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::fs::write(root.join("dir/b.bin"), [0u8, 1, 2, 255]).unwrap();
        std::fs::write(root.join("z"), "").unwrap();

        assert_eq!(TreeDigest::of_tree(root).unwrap(), TreeDigest::parse(PINNED).unwrap());
    }
}
