//! Shared build diagnostics for CLI, GUI and editor consumers.
//!
//! These describe tool output, not command success or the library's [`crate::Error`].
//! Codes and enum values are locale-independent machine identifiers; do not
//! rename or reuse them for a different meaning. Text uses [`Message`] and the
//! existing renderer. JSON includes absent values as `null` and empty lists as
//! `[]`, so consumers need not infer whether a field was collected.

use std::num::NonZeroU32;
use std::path::PathBuf;

use serde::Serialize;

use crate::Message;

mod cpp;
pub use cpp::parse_cpp;
mod python;
pub use python::parse_python;

/// Stable rule identifiers, shared by tools reporting the same kind of error.
/// Adding a code does not imply that its parser has been implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Code {
    Unknown,
    SyntaxError,
    TypeError,
    MissingHeader,
    PackageNotFound,
    LibraryNotFound,
    UndefinedReference,
    IndentationError,
    TabError,
    ModuleNotFound,
    ImportError,
    RuntimeError,
    ToolFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Category {
    Syntax,
    Type,
    HeaderSearch,
    PackageDiscovery,
    Linking,
    Import,
    Runtime,
    Execution,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// A position reported by the tool, not necessarily the root cause's location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    /// Preserve the reported path, including relative paths; do not canonicalize.
    /// Relative paths are interpreted against the originating command's cwd.
    pub file: PathBuf,
    /// One-based when known. Missing or zero/unknown tool coordinates become None.
    pub line: Option<NonZeroU32>,
    /// One-based when known; retain the tool's column units (bytes/characters).
    pub column: Option<NonZeroU32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub code: Code,
    pub category: Category,
    pub severity: Severity,
    /// Tool name as identified from the command or output; None if unknown.
    pub tool: Option<String>,
    /// None when no file was reported; never invent a file or coordinates.
    pub location: Option<Location>,
    /// Original error excerpt, without translation or rewriting.
    pub raw: String,
    /// Include chains, template notes, traceback frames, chained exceptions or
    /// downstream failures, in reported order. Association does not prove cause.
    pub related: Vec<Diagnostic>,
    /// Explains only facts explicitly reported by the tool, not inferred causes.
    pub explanation: Message,
    /// Hypotheses requiring verification, kept separate from reported facts.
    pub possible_causes: Vec<Message>,
    /// Suggested checks or actions; diagnostics never execute these themselves.
    pub suggestions: Vec<Message>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unknown() -> Diagnostic {
        Diagnostic {
            code: Code::Unknown,
            category: Category::Unknown,
            severity: Severity::Note,
            tool: None,
            location: None,
            raw: "unrecognized tool output".into(),
            related: vec![],
            explanation: Message::text("unrecognized tool output"),
            possible_causes: vec![],
            suggestions: vec![],
        }
    }

    #[test]
    fn json_preserves_facts_hypotheses_and_related_context() {
        let diagnostic = Diagnostic {
            code: Code::MissingHeader,
            category: Category::HeaderSearch,
            severity: Severity::Error,
            tool: Some("gcc".into()),
            location: Some(Location {
                file: "src with spaces/main.cpp".into(),
                line: NonZeroU32::new(3),
                column: NonZeroU32::new(10),
            }),
            raw: "\u{1b}[31mfatal error: demo.h: No such file or directory\u{1b}[0m".into(),
            related: vec![unknown()],
            explanation: crate::msg!("diagnostic.header_missing", header = "demo.h"),
            possible_causes: vec![crate::msg!("diagnostic.header_search_path")],
            suggestions: vec![crate::msg!("diagnostic.check_include_path")],
        };
        assert_eq!(
            serde_json::to_value(&diagnostic).unwrap(),
            json!({
                "code": "missing_header", "category": "header_search",
                "severity": "error", "tool": "gcc",
                "location": {"file": "src with spaces/main.cpp", "line": 3, "column": 10},
                "raw": diagnostic.raw,
                "related": [{
                    "code": "unknown", "category": "unknown", "severity": "note",
                    "tool": null, "location": null, "raw": "unrecognized tool output",
                    "related": [], "explanation": "unrecognized tool output",
                    "possible_causes": [], "suggestions": []
                }],
                "explanation": diagnostic.explanation.to_string(),
                "possible_causes": [diagnostic.possible_causes[0].to_string()],
                "suggestions": [diagnostic.suggestions[0].to_string()]
            })
        );
        assert_eq!(
            diagnostic.explanation.key_name(),
            Some("diagnostic.header_missing")
        );
    }

    #[test]
    fn missing_coordinates_are_not_fabricated() {
        let mut diagnostic = unknown();
        assert!(serde_json::to_value(&diagnostic).unwrap()["location"].is_null());
        diagnostic.location = Some(Location {
            file: "CMakeLists.txt".into(),
            line: NonZeroU32::new(12),
            column: None,
        });
        assert_eq!(
            serde_json::to_value(&diagnostic).unwrap()["location"],
            json!({"file": "CMakeLists.txt", "line": 12, "column": null})
        );
        diagnostic.location.as_mut().unwrap().line = NonZeroU32::new(0);
        assert_eq!(
            serde_json::to_value(&diagnostic).unwrap()["location"],
            json!({"file": "CMakeLists.txt", "line": null, "column": null})
        );
    }
}
