//! Default Python traceback and syntax diagnostics. Parse one command/package
//! stream at a time: interleaved tracebacks cannot be reliably disentangled.
use super::{cpp::strip_ansi, Category, Code, Diagnostic, Location, Severity};
use crate::{msg, Message};
use std::num::NonZeroU32;

/// Parse Python's default text output, preserving ANSI in raw excerpts.
///
/// `interpreter` is optional caller-supplied context for the failing build or
/// verification command (an executable path or selected environment label), NOT
/// the Python running rospm's parent shell. No interpreter is executed here.
/// Import suggestions ask the user to check that command's sys.executable and
/// sys.path; absence from that environment does not prove global absence.
/// Unknown formats (including ExceptionGroup trees) are left to the raw log.
/// Carets are preserved but not converted to columns: source indentation and
/// tab expansion make that conversion ambiguous in default traceback output.
pub fn parse_python(output: &str, interpreter: Option<&str>) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    let mut raw: Vec<&str> = Vec::new();
    let mut frames: Vec<Diagnostic> = Vec::new();
    let mut active = false;
    let mut chain = false;
    let mut can_chain = false;
    for original in output.lines() {
        let clean = strip_ansi(original);
        let line = clean.trim_end();
        if matches!(
            line,
            "During handling of the above exception, another exception occurred:"
                | "The above exception was the direct cause of the following exception:"
        ) && can_chain
        {
            chain = true;
            raw.push(original);
            continue;
        }
        if line == "Traceback (most recent call last):" {
            if !chain {
                raw.clear();
            }
            frames.clear();
            active = true;
            can_chain = false;
            raw.push(original);
            continue;
        }
        if let Some(location) = frame_location(line) {
            if !active && !chain {
                raw.clear();
            }
            active = true;
            can_chain = false;
            raw.push(original);
            frames.push(Diagnostic {
                code: Code::Unknown,
                category: Category::Runtime,
                severity: Severity::Note,
                tool: Some("python".into()),
                location: Some(location),
                raw: original.into(),
                related: vec![],
                explanation: msg!("diagnostic.python_frame"),
                possible_causes: vec![],
                suggestions: vec![],
            });
            continue;
        }
        if let Some((kind, message)) = exception(line, active) {
            raw.push(original);
            let mut d = diagnose(kind, message, interpreter);
            d.location = frames.last().and_then(|f| f.location.clone());
            if chain {
                if let Some(previous) = out.pop() {
                    d.related.push(previous);
                }
            }
            d.related.append(&mut frames);
            d.raw = raw.join("\n");
            out.push(d);
            raw.clear();
            active = false;
            chain = false;
            can_chain = true;
            continue;
        }
        if active && (line.starts_with([' ', '\t']) || line.is_empty()) {
            raw.push(original);
            if let Some(frame) = frames.last_mut() {
                frame.raw.push('\n');
                frame.raw.push_str(original);
            }
        } else if line.is_empty() {
            // CPython inserts blank lines around exception-chain separators.
            if chain {
                raw.push(original);
            }
        } else {
            raw.clear();
            frames.clear();
            active = false;
            chain = false;
            can_chain = false;
        }
    }
    out
}

fn frame_location(line: &str) -> Option<Location> {
    let rest = line.strip_prefix("  File \"")?;
    let (file, tail) = rest.rsplit_once("\", line ")?;
    let number = tail.split_once(", in ").map_or(tail, |(n, _)| n);
    Some(Location {
        file: file.into(),
        line: NonZeroU32::new(number.parse().ok()?),
        column: None,
    })
}

fn exception(line: &str, active: bool) -> Option<(&str, &str)> {
    let (kind, message) = line
        .split_once(':')
        .map_or((line, ""), |(k, m)| (k, m.trim()));
    if kind.is_empty()
        || !kind.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .enumerate()
                    .all(|(i, c)| c == '_' || c.is_alphabetic() || (i > 0 && c.is_ascii_digit()))
        })
    {
        return None;
    }
    let known = matches!(
        kind,
        "SyntaxError"
            | "IndentationError"
            | "TabError"
            | "ModuleNotFoundError"
            | "ImportError"
            | "TypeError"
            | "ValueError"
            | "NameError"
            | "AttributeError"
            | "KeyError"
            | "IndexError"
            | "ZeroDivisionError"
            | "RuntimeError"
            | "OSError"
            | "FileNotFoundError"
            | "PermissionError"
            | "AssertionError"
            | "NotImplementedError"
            | "RecursionError"
            | "UnboundLocalError"
            | "StopIteration"
            | "KeyboardInterrupt"
            | "SystemExit"
    );
    (active || known).then_some((kind, message))
}

fn diagnose(kind: &str, message: &str, interpreter: Option<&str>) -> Diagnostic {
    let (code, category, suggestion) = match kind {
        "SyntaxError" => (
            Code::SyntaxError,
            Category::Syntax,
            "diagnostic.check_reported_line",
        ),
        "IndentationError" => (
            Code::IndentationError,
            Category::Syntax,
            "diagnostic.python_indentation",
        ),
        "TabError" => (Code::TabError, Category::Syntax, "diagnostic.python_tabs"),
        "ModuleNotFoundError" => (
            Code::ModuleNotFound,
            Category::Import,
            "diagnostic.python_check_environment",
        ),
        "ImportError" => (
            Code::ImportError,
            Category::Import,
            "diagnostic.python_check_import",
        ),
        _ => (
            Code::RuntimeError,
            Category::Runtime,
            "diagnostic.python_check_runtime",
        ),
    };
    let mut d = Diagnostic {
        code,
        category,
        severity: Severity::Error,
        tool: Some("python".into()),
        location: None,
        raw: String::new(),
        related: vec![],
        explanation: msg!(
            "diagnostic.python_exception",
            exception = kind,
            message = message
        ),
        possible_causes: vec![],
        suggestions: vec![Message::key(suggestion)],
    };
    if category == Category::Import {
        d.possible_causes
            .push(msg!("diagnostic.python_import_causes"));
        if kind == "ImportError" {
            d.suggestions
                .push(msg!("diagnostic.python_check_environment"));
        }
        if let Some(interpreter) = interpreter {
            d.suggestions.push(msg!(
                "diagnostic.python_selected_interpreter",
                interpreter = interpreter
            ));
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_indentation_and_tabs_preserve_locations_and_carets() {
        for (kind, code) in [
            ("SyntaxError", Code::SyntaxError),
            ("IndentationError", Code::IndentationError),
            ("TabError", Code::TabError),
        ] {
            let log = format!("  File \"src with spaces/节点.py\", line 7\n    bad syntax\n        ^\n\x1b[31m{kind}: example\x1b[0m\n");
            let result = parse_python(&log, None);
            assert_eq!(result.len(), 1);
            let d = &result[0];
            assert_eq!(d.code, code);
            assert_eq!(
                d.location.as_ref().unwrap().file.to_str(),
                Some("src with spaces/节点.py")
            );
            assert_eq!(d.location.as_ref().unwrap().line.unwrap().get(), 7);
            assert_eq!(d.location.as_ref().unwrap().column, None);
            assert_eq!(d.raw, log.trim_end());
            assert!(d.related[0].raw.contains('^'));
        }
    }

    #[test]
    fn chain_retains_exceptions_project_frames_and_order() {
        let log = "Traceback (most recent call last):\n  File \"project/main.py\", line 2, in main\n    load()\n  File \"vendor/lib.py\", line 9, in load\n    import absent\nModuleNotFoundError: No module named 'absent'\n\nThe above exception was the direct cause of the following exception:\n\nTraceback (most recent call last):\n  File \"project/main.py\", line 4, in main\n    raise ValueError('bad') from e\nValueError: bad\n";
        let ds = parse_python(log, Some("conda -n robot"));
        assert_eq!(ds.len(), 1);
        let d = &ds[0];
        assert_eq!(d.code, Code::RuntimeError);
        assert!(d.explanation.to_string().contains("ValueError"));
        assert_eq!(d.related.len(), 2);
        let cause = &d.related[0];
        assert_eq!(cause.code, Code::ModuleNotFound);
        assert_eq!(cause.related.len(), 2);
        assert_eq!(
            cause.related[0].location.as_ref().unwrap().file.to_str(),
            Some("project/main.py")
        );
        assert!(cause
            .suggestions
            .iter()
            .any(|m| m.to_string().contains("conda -n robot")));
        assert!(!cause.explanation.to_string().contains("installed"));
        assert!(d.raw.contains("direct cause"));
        let implicit = parse_python(
            &log.replace(
                "The above exception was the direct cause of the following exception:",
                "During handling of the above exception, another exception occurred:",
            ),
            None,
        );
        assert_eq!(implicit[0].related[0].code, Code::ModuleNotFound);
    }

    #[test]
    fn common_import_errors_and_missing_location() {
        for message in ["cannot import name 'thing' from partially initialized module 'demo' (most likely due to a circular import)",
            "libexample.so: cannot open shared object file: No such file or directory",
            "/opt/extension.so: undefined symbol: example"] {
            let ds = parse_python(&format!("ImportError: {message}"), None);
            assert_eq!(ds[0].code, Code::ImportError);
            assert_eq!(ds[0].location, None);
            assert!(ds[0].explanation.to_string().contains(message));
            assert!(!ds[0].possible_causes.is_empty());
            assert_eq!(ds[0].suggestions.len(), 2);
        }
    }

    #[test]
    fn boundaries_unknown_errors_and_success() {
        assert!(parse_python(
            "Starting >>> demo\nall passed\nFinished <<< demo\n[INFO] no errors\n",
            None
        )
        .is_empty());
        assert!(parse_python("example.CustomProblem: text\n", None).is_empty());
        assert!(parse_python("  File \"broken\", line not-a-number\n", None).is_empty());
        let ds = parse_python("Traceback (most recent call last):\n  File \"a.py\", line 0, in f\nexample.CustomProblem: detail\ncommand boundary\nSyntaxError: invalid syntax", None);
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[0].code, Code::RuntimeError);
        assert!(ds[0]
            .explanation
            .to_string()
            .contains("example.CustomProblem"));
        assert_eq!(ds[0].location.as_ref().unwrap().line, None);
        assert!(ds[1].location.is_none());
        assert!(ds[1].related.is_empty());
        let ds = parse_python("ValueError: first\ncommand boundary\nThe above exception was the direct cause of the following exception:\nTypeError: second", None);
        assert_eq!(ds.len(), 2);
        assert!(ds[1].related.is_empty());
    }

    #[test]
    fn real_python_failures() {
        // No ROS, installed modules, network or user-site dependencies needed.
        for (source, code, kind) in [
            ("if True print('bad')", Code::SyntaxError, "SyntaxError"),
            ("if True:\npass", Code::IndentationError, "IndentationError"),
            ("if True:\n\tpass\n        pass", Code::TabError, "TabError"),
            ("import rospm_deliberately_absent_test_module", Code::ModuleNotFound, "ModuleNotFoundError"),
            ("from sys import rospm_deliberately_absent_name", Code::ImportError, "ImportError"),
            ("def f():\n    return 1 / 0\nf()", Code::RuntimeError, "ZeroDivisionError"),
            ("try:\n    {}['missing']\nexcept KeyError as e:\n    raise ValueError('wrapped') from e", Code::RuntimeError, "ValueError"),
        ] {
            let output = std::process::Command::new("python3").args(["-I", "-S", "-c", source]).output().expect("python3 is required for traceback regression tests");
            assert!(!output.status.success());
            let log = String::from_utf8(output.stderr).unwrap();
            let ds = parse_python(&log, Some("python3 -I -S"));
            assert_eq!(ds.len(), 1, "{log}");
            assert_eq!(ds[0].code, code, "{log}");
            assert!(ds[0].explanation.to_string().contains(kind));
            assert_eq!(ds[0].location.as_ref().unwrap().file.to_str(), Some("<string>"));
            assert!(ds[0].raw.contains(kind));
        }
    }
}
