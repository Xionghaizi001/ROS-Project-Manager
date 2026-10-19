//! Versioned, offline defaults for apt target replacements. Data changes are
//! reviewed and shipped with rospm; a manifest always has the final say.

use std::sync::OnceLock;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::{msg, platform::Platform, Message};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinReplacements {
    pub schema_version: u32,
    pub revision: String,
    pub target: IndexMap<Platform, IndexMap<String, String>>,
}

/// Read once from the data bundled into the binary; never downloads or writes.
pub fn builtin() -> &'static BuiltinReplacements {
    static TABLE: OnceLock<BuiltinReplacements> = OnceLock::new();
    TABLE.get_or_init(|| {
        let table: BuiltinReplacements =
            toml::from_str(include_str!("../data/apt-replacements.toml"))
                .expect("bundled apt replacements must be valid");
        assert_eq!(table.schema_version, 1, "unsupported replacement schema");
        table
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementSource {
    Builtin,
    Manifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AptReplacement {
    pub from: String,
    pub to: String,
    pub source: ReplacementSource,
}

impl AptReplacement {
    pub fn notice(&self, platform: Platform) -> Message {
        match self.source {
            ReplacementSource::Builtin => msg!(
                "replacement.builtin",
                platform = platform,
                from = self.from,
                to = self.to,
                revision = builtin().revision
            ),
            ReplacementSource::Manifest => msg!(
                "replacement.manifest",
                platform = platform,
                from = self.from,
                to = self.to
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AptResolution {
    pub packages: Vec<String>,
    /// Only changes that survive exclusion, in dependency order, without duplicates.
    pub replacements: Vec<AptReplacement>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_table_has_a_revision_and_valid_non_chaining_package_rules() {
        let table = builtin();
        assert!(!table.revision.trim().is_empty());
        assert!(!table.target.is_empty());
        for rules in table.target.values() {
            assert!(!rules.is_empty());
            for (from, to) in rules {
                for name in [from, to] {
                    assert!(name.starts_with(|c: char| c.is_ascii_lowercase()));
                    assert!(name.bytes().all(|c| c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || b"+.-".contains(&c)));
                }
                assert_ne!(from, to);
                assert!(!rules.contains_key(to), "built-in rules must not chain");
            }
        }
    }
}
