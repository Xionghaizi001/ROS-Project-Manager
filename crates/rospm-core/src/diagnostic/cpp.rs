//! First C++ toolchain rules: GCC / Clang compile errors, CMake errors and
//! GNU ld / lld link errors, parsed from their default text output.
//!
//! Structured formats (`-fdiagnostics-format=json`, SARIF) would replace the
//! human-readable output the user watches live, so builds keep text output
//! and this parser reads it. Unrecognized lines are skipped here; keeping the
//! raw log for unknown failures is the caller's job.

use std::num::NonZeroU32;

use super::{Category, Code, Diagnostic, Location, Severity};
use crate::{msg, Message};

/// Parse a build log (ANSI colors allowed) into top-level diagnostics in
/// reported order. Notes, include chains, template instantiation context and
/// source excerpts are attached to the error they belong to.
pub fn parse_cpp(output: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<Diagnostic> = Vec::new();
    // Context lines GCC / Clang / ld print *before* the error they describe.
    let mut pending: Vec<Diagnostic> = Vec::new();
    // Whether excerpts / notes may still attach to `out.last()`.
    let mut open = false;
    // Whether excerpts belong to the last `note:` rather than the error itself.
    let mut in_note = false;
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line = strip_ansi(raw);
        let line = line.trim_end();
        i += 1;

        if let Some(rest) = line.strip_prefix("CMake Error") {
            let start = i - 1;
            while i < lines.len() {
                let l = strip_ansi(lines[i]);
                if l.is_empty() || l.starts_with([' ', '\t']) || l.starts_with("Call Stack") {
                    i += 1;
                } else {
                    break;
                }
            }
            let block: Vec<&str> = lines[start..i].to_vec();
            let end = block
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .unwrap_or(0);
            out.push(cmake(rest, &block[..=end]));
            pending.clear();
            open = false;
            continue;
        }

        // Source excerpt (`    3 |   return a;`) or lld `>>> referenced by`.
        if open && (is_excerpt(line) || line.trim_start().starts_with(">>>")) {
            let last = out.last_mut().unwrap();
            let target = match last.related.last_mut() {
                Some(note) if in_note => note,
                _ => last,
            };
            target.raw.push('\n');
            target.raw.push_str(raw);
            continue;
        }

        if let Some(d) = linker(line, raw) {
            let mut d = d;
            d.related = std::mem::take(&mut pending);
            out.push(d);
            open = true;
            in_note = false;
            continue;
        }

        if let Some((location, kind, message)) = compiler_line(line) {
            match kind {
                "error" | "fatal error" => {
                    let mut d = compiler(location, kind, message, raw);
                    d.related = std::mem::take(&mut pending);
                    out.push(d);
                    open = true;
                    in_note = false;
                }
                "note" if open => {
                    out.last_mut()
                        .unwrap()
                        .related
                        .push(context(raw, Some(location)));
                    in_note = true;
                }
                // Warnings are not first-batch rules; -Werror ones arrive as `error:`.
                _ => {}
            }
            continue;
        }

        if is_context(line) {
            let location = context_location(line);
            pending.push(context(raw, location));
            open = false;
            continue;
        }

        // shortcut: Clang excerpts have no `|` gutter, so they are neither added
        // to `raw` nor allowed to close the error; recognize them if a GUI needs them.
        pending.clear();
    }
    out
}

/// `file:line[:col]: <kind>: <message>`, the GCC / Clang layout.
fn compiler_line(line: &str) -> Option<(Location, &str, &str)> {
    let (location, rest) = split_location(line)?;
    let rest = rest.trim_start();
    for kind in ["fatal error", "error", "warning", "note"] {
        if let Some(message) = rest.strip_prefix(kind).and_then(|r| r.strip_prefix(':')) {
            return Some((location, kind, message.trim()));
        }
    }
    None
}

fn compiler(location: Location, kind: &str, message: &str, raw: &str) -> Diagnostic {
    let header = missing_header(kind, message);
    let (code, category) = if header.is_some() {
        (Code::MissingHeader, Category::HeaderSearch)
    } else {
        classify(message)
    };
    let (explanation, possible_causes, suggestions) = match code {
        Code::MissingHeader => (
            msg!("diagnostic.header_missing", header = header.unwrap()),
            vec![msg!("diagnostic.header_search_path")],
            vec![msg!("diagnostic.check_include_path")],
        ),
        Code::SyntaxError => (
            msg!("diagnostic.syntax_error", message = message),
            vec![],
            vec![msg!("diagnostic.check_reported_line")],
        ),
        Code::TypeError => (
            msg!("diagnostic.type_error", message = message),
            vec![],
            vec![msg!("diagnostic.check_types")],
        ),
        _ => (
            msg!("diagnostic.compiler_error", message = message),
            vec![],
            vec![msg!("diagnostic.check_reported_line")],
        ),
    };
    // Only the header forms identify the compiler; other lines look the same.
    let tool = match () {
        _ if message.ends_with("file not found") => Some("clang".into()),
        _ if header.is_some() => Some("gcc".into()),
        _ => None,
    };
    Diagnostic {
        code,
        category,
        severity: Severity::Error,
        tool,
        location: Some(location),
        raw: raw.to_string(),
        related: vec![],
        explanation,
        possible_causes,
        suggestions,
    }
}

/// GCC: `fatal error: x.h: No such file or directory` (strerror is localized);
/// Clang: `fatal error: 'x.h' file not found`.
fn missing_header<'a>(kind: &str, message: &'a str) -> Option<&'a str> {
    if kind != "fatal error" {
        return None;
    }
    if let Some(name) = message.strip_suffix(" file not found") {
        return Some(name.trim_matches(['\'', '"']));
    }
    // GCC uses `<file>: <strerror>` fatal errors only for files it failed to open.
    let (name, _) = message.split_once(": ")?;
    (!name.is_empty() && !name.contains(' ')).then_some(name)
}

fn classify(message: &str) -> (Code, Category) {
    const SYNTAX: &[&str] = &[
        "expected ",
        "stray ",
        "missing terminating",
        "unterminated",
        "extraneous closing brace",
        "expected unqualified-id",
    ];
    const TYPE: &[&str] = &[
        "cannot convert",
        "invalid conversion",
        "no matching function",
        "no matching constructor",
        "no matching member function",
        "no viable conversion",
        "no viable overloaded",
        "incompatible type",
        "invalid operands",
        "no member named",
        "has no member named",
        "cannot initialize",
        "invalid initialization",
        "conversion from",
        "assigning to",
    ];
    if SYNTAX.iter().any(|p| message.starts_with(p)) {
        (Code::SyntaxError, Category::Syntax)
    } else if TYPE.iter().any(|p| message.contains(p)) {
        (Code::TypeError, Category::Type)
    } else {
        (Code::Unknown, Category::Unknown)
    }
}

/// GNU ld / lld library and symbol errors. `raw` keeps the original line.
fn linker(line: &str, raw: &str) -> Option<Diagnostic> {
    let (tool, rest) = linker_prefix(line);
    let base = |code, category, explanation, cause, suggestion, location| Diagnostic {
        code,
        category,
        severity: Severity::Error,
        tool: tool.map(Into::into),
        location,
        raw: raw.to_string(),
        related: vec![],
        explanation,
        possible_causes: vec![cause],
        suggestions: vec![suggestion],
    };
    // `cannot find -lfoo` (also localized, e.g. `找不到 -lfoo`), lld `unable to find library -lfoo`.
    if tool.is_some() {
        if let Some((_, lib)) = rest
            .split_once(" -l")
            .filter(|(head, _)| !head.contains("undefined") && !head.starts_with("warning"))
        {
            let lib = lib.split([':', ' ']).next().unwrap_or(lib);
            return Some(base(
                Code::LibraryNotFound,
                Category::Linking,
                msg!("diagnostic.library_not_found", library = lib),
                msg!("diagnostic.library_search_path"),
                msg!("diagnostic.check_link_libraries"),
                None,
            ));
        }
    }
    let (location, symbol) =
        if let Some((head, symbol)) = rest.split_once("undefined reference to ") {
            (object_location(head), symbol)
        } else {
            (None, rest.strip_prefix("error: undefined symbol: ")?)
        };
    let symbol = symbol.trim_matches(['`', '\'', '‘', '’']);
    Some(base(
        Code::UndefinedReference,
        Category::Linking,
        msg!("diagnostic.undefined_reference", symbol = symbol),
        msg!("diagnostic.symbol_not_linked"),
        msg!("diagnostic.check_link_libraries"),
        location,
    ))
}

/// Split `/usr/bin/ld: …`, `ld.lld: …` off; unprefixed lines keep `None`.
fn linker_prefix(line: &str) -> (Option<&str>, &str) {
    if let Some((head, rest)) = line.split_once(": ") {
        let name = head.rsplit('/').next().unwrap_or(head);
        if ["ld", "ld.lld", "ld.gold", "ld.bfd", "lld", "mold"].contains(&name) {
            return (Some(name), rest);
        }
    }
    (None, line)
}

/// `main.cpp:(.text+0x9): ` or `/x/main.cpp:2: ` in front of a linker message.
fn object_location(head: &str) -> Option<Location> {
    let head = head.trim_end().strip_suffix(':')?;
    if let Some((file, _)) = head.split_once(":(") {
        return Some(Location {
            file: file.into(),
            line: None,
            column: None,
        });
    }
    let (file, line) = head.rsplit_once(':')?;
    Some(Location {
        file: file.into(),
        line: line.parse().ok(),
        column: None,
    })
}

fn cmake(header: &str, block: &[&str]) -> Diagnostic {
    // `CMake Error at <file>:<line> (<command>):`, or `CMake Error: …` without a place.
    let at = header
        .strip_prefix(" at ")
        .or_else(|| header.strip_prefix(" in "));
    let (location, command) = match at.and_then(|h| h.strip_suffix(':')) {
        Some(h) => {
            let (place, command) = match h.rsplit_once(" (") {
                Some((p, c)) => (p, c.strip_suffix(')')),
                None => (h, None),
            };
            let location = match place.rsplit_once(':') {
                Some((file, line)) if line.parse::<u32>().is_ok() => Location {
                    file: file.into(),
                    line: line.parse().ok(),
                    column: None,
                },
                _ => Location {
                    file: place.into(),
                    line: None,
                    column: None,
                },
            };
            (Some(location), command)
        }
        None => (None, None),
    };
    // CMake wraps messages at ~80 columns; join before searching for names.
    let body: String = block[1..]
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let package = between(&body, "provided by \"", "\"")
        .or_else(|| between(&body, "Could NOT find ", " ").map(|p| p.trim_end_matches(':')));
    let raw = block.join("\n");
    let mut d = Diagnostic {
        code: Code::Unknown,
        category: Category::Unknown,
        severity: Severity::Error,
        tool: Some("cmake".into()),
        location,
        raw,
        related: vec![],
        explanation: msg!("diagnostic.cmake_error"),
        possible_causes: vec![],
        suggestions: vec![msg!("diagnostic.check_reported_line")],
    };
    if command == Some("find_package") || package.is_some() {
        d.code = Code::PackageNotFound;
        d.category = Category::PackageDiscovery;
        d.possible_causes = vec![msg!("diagnostic.package_prefix")];
        let package = package.unwrap_or("?");
        d.explanation = msg!("diagnostic.package_not_found", package = package);
        d.suggestions = vec![msg!("diagnostic.check_find_package", package = package)];
    }
    d
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = &s[s.find(start)? + start.len()..];
    Some(&rest[..rest.find(end)?]).filter(|p| !p.is_empty())
}

/// Lines that describe where the next error happens rather than an error.
fn is_context(line: &str) -> bool {
    line.starts_with("In file included from ")
        || line.trim_start().starts_with("from ")
        || line.ends_with("required from here")
        || line.contains(": required from ")
        || line.contains(": In instantiation of ")
        || line.contains(": In function ")
        || line.contains(": In member function ")
        || line.contains(": In constructor ")
        || line.contains(": In destructor ")
        || line.contains(": In lambda function")
        || line.contains(": in function `")
}

fn context_location(line: &str) -> Option<Location> {
    let l = line.trim_start();
    let l = l
        .strip_prefix("In file included from ")
        .or_else(|| l.strip_prefix("from "))
        .unwrap_or(l);
    let l = l.trim_end_matches([',', ':']);
    // Include chain entries are `file:line` without a column.
    split_location(&format!("{l}: "))
        .map(|(loc, _)| loc)
        .or_else(|| split_location(line).map(|(loc, _)| loc))
        .or_else(|| {
            // `file: In function …` / `ld: obj.o: in function …` name only a file.
            let (_, l) = linker_prefix(line);
            let (file, _) = l.split_once(": In ").or_else(|| l.split_once(": in "))?;
            Some(Location {
                file: file.into(),
                line: None,
                column: None,
            })
        })
}

fn context(raw: &str, location: Option<Location>) -> Diagnostic {
    let text = strip_ansi(raw);
    Diagnostic {
        code: Code::Unknown,
        category: Category::Unknown,
        severity: Severity::Note,
        tool: None,
        location,
        raw: raw.to_string(),
        related: vec![],
        explanation: Message::text(text.trim()),
        possible_causes: vec![],
        suggestions: vec![],
    }
}

/// `    3 |   code` / `      |   ^~~` source excerpt lines from GCC / Clang.
fn is_excerpt(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit());
    line.starts_with(' ') && (t.trim_start().starts_with('|') || t.trim().starts_with('^'))
}

/// Find the first `file:line[:col]: ` and return the location and the rest.
/// Splits on the first such match so paths may contain spaces.
fn split_location(line: &str) -> Option<(Location, &str)> {
    let bytes = line.as_bytes();
    for (i, _) in line.match_indices(':') {
        if i == 0 {
            continue;
        }
        let digits = |from: usize| {
            bytes[from..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count()
        };
        let n = digits(i + 1);
        if n == 0 {
            continue;
        }
        let line_no = &line[i + 1..i + 1 + n];
        let mut end = i + 1 + n;
        let mut column = None;
        if bytes.get(end) == Some(&b':') && digits(end + 1) > 0 {
            let m = digits(end + 1);
            column = line[end + 1..end + 1 + m]
                .parse::<u32>()
                .ok()
                .and_then(NonZeroU32::new);
            end += 1 + m;
        }
        if bytes.get(end) == Some(&b':') && matches!(bytes.get(end + 1), Some(b' ') | None) {
            let location = Location {
                file: line[..i].into(),
                line: line_no.parse::<u32>().ok().and_then(NonZeroU32::new),
                column,
            };
            return Some((location, &line[(end + 1).min(line.len())..]));
        }
    }
    None
}

/// Remove CSI (`ESC [ … final`) and OSC (`ESC ] … BEL|ESC \`) sequences.
pub(super) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(d: &Diagnostic) -> (String, Option<u32>, Option<u32>) {
        let l = d.location.as_ref().unwrap();
        (
            l.file.display().to_string(),
            l.line.map(NonZeroU32::get),
            l.column.map(NonZeroU32::get),
        )
    }

    // Captured from GCC 11.4 with -fdiagnostics-color=always (Ubuntu 22.04).
    const GCC_SYNTAX: &str = "\u{1b}[01m\u{1b}[Ksyn.cpp:\u{1b}[m\u{1b}[K In function ‘\u{1b}[01m\u{1b}[Kint main()\u{1b}[m\u{1b}[K’:\n\u{1b}[01m\u{1b}[Ksyn.cpp:4:3:\u{1b}[m\u{1b}[K \u{1b}[01;31m\u{1b}[Kerror: \u{1b}[m\u{1b}[Kexpected ‘\u{1b}[01m\u{1b}[K,\u{1b}[m\u{1b}[K’ or ‘\u{1b}[01m\u{1b}[K;\u{1b}[m\u{1b}[K’ before ‘\u{1b}[01m\u{1b}[Kreturn\u{1b}[m\u{1b}[K’\n    4 |   \u{1b}[01;31m\u{1b}[Kreturn\u{1b}[m\u{1b}[K a;\n      |   \u{1b}[01;31m\u{1b}[K^~~~~~\u{1b}[m\u{1b}[K\n";

    #[test]
    fn gcc_syntax_error_with_ansi_context_and_excerpt() {
        let d = parse_cpp(GCC_SYNTAX);
        assert_eq!(d.len(), 1);
        let d = &d[0];
        assert_eq!(
            (d.code, d.category, d.severity),
            (Code::SyntaxError, Category::Syntax, Severity::Error)
        );
        assert_eq!(loc(d), ("syn.cpp".into(), Some(4), Some(3)));
        assert!(d.raw.starts_with("\u{1b}[01m"), "raw keeps ANSI");
        assert!(
            d.raw.contains("    4 |   \u{1b}[01;31m"),
            "excerpt attached: {:?}",
            d.raw
        );
        assert_eq!(d.related.len(), 1);
        assert!(d.related[0].raw.contains("In function"));
        assert_eq!(d.explanation.key_name(), Some("diagnostic.syntax_error"));
        assert_eq!(
            d.explanation.args()[0].1,
            "expected ‘,’ or ‘;’ before ‘return’"
        );
    }

    #[test]
    fn gcc_type_error_keeps_template_instantiation_chain() {
        let log = r#"ty.cpp: In instantiation of ‘int f(T) [with T = std::__cxx11::basic_string<char>]’:
ty.cpp:4:37:   required from here
ty.cpp:3:44: error: cannot convert ‘std::__cxx11::basic_string<char>’ to ‘int’ in initialization
    3 | template <typename T> int f(T t) { int x = t; return x; }
      |                                            ^
      |                                            |
      |                                            std::__cxx11::basic_string<char>
"#;
        let d = parse_cpp(log);
        assert_eq!(d.len(), 1);
        assert_eq!(
            (d[0].code, d[0].category),
            (Code::TypeError, Category::Type)
        );
        assert_eq!(loc(&d[0]), ("ty.cpp".into(), Some(3), Some(44)));
        let related: Vec<_> = d[0].related.iter().map(loc).collect();
        assert_eq!(
            related,
            [
                ("ty.cpp".into(), None, None),
                ("ty.cpp".into(), Some(4), Some(37))
            ]
        );
        assert_eq!(d[0].raw.lines().count(), 5);
    }

    #[test]
    fn missing_header_gcc_localized_and_clang_with_include_chain_and_spaces() {
        // GCC under zh_CN localizes strerror only.
        let gcc = r#"In file included from sp ace/hdr.cpp:1:
sp ace/inc.h:1:10: fatal error: nothere.h: 没有那个文件或目录
    1 | #include <nothere.h>
      |          ^~~~~~~~~~~
compilation terminated.
"#;
        let d = parse_cpp(gcc);
        assert_eq!(d.len(), 1);
        assert_eq!(
            (d[0].code, d[0].category),
            (Code::MissingHeader, Category::HeaderSearch)
        );
        assert_eq!(d[0].tool.as_deref(), Some("gcc"));
        assert_eq!(loc(&d[0]), ("sp ace/inc.h".into(), Some(1), Some(10)));
        assert_eq!(d[0].explanation.args()[0].1, "nothere.h");
        assert_eq!(d[0].raw.lines().count(), 3);
        assert_eq!(
            loc(&d[0].related[0]),
            ("sp ace/hdr.cpp".into(), Some(1), None)
        );

        // Clang layout (documented format; no clang on the capture host).
        let clang = "In file included from src/a.cpp:1:\n\
src/a.h:2:10: fatal error: 'foo/bar.hpp' file not found\n\
#include <foo/bar.hpp>\n\
         ^~~~~~~~~~~~~\n\
1 error generated.\n";
        let d = parse_cpp(clang);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Code::MissingHeader);
        assert_eq!(d[0].tool.as_deref(), Some("clang"));
        assert_eq!(d[0].explanation.args()[0].1, "foo/bar.hpp");
        assert_eq!(d[0].related.len(), 1);
    }

    #[test]
    fn clang_notes_attach_to_previous_error() {
        let log = "a.cpp:5:3: error: no matching function for call to 'f'\n  f(1, 2);\n  ^\n\
a.cpp:1:6: note: candidate function not viable: requires single argument 'x', but 2 arguments were provided\n\
void f(int x);\n     ^\n1 error generated.\n";
        let d = parse_cpp(log);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Code::TypeError);
        assert_eq!(d[0].related.len(), 1);
        assert_eq!(loc(&d[0].related[0]), ("a.cpp".into(), Some(1), Some(6)));
    }

    #[test]
    fn unrecognized_compiler_error_keeps_location_without_guessing() {
        let d = parse_cpp("x.cpp:7:1: error: ‘foo’ was not declared in this scope\n");
        assert_eq!(
            (d[0].code, d[0].category),
            (Code::Unknown, Category::Unknown)
        );
        assert_eq!(loc(&d[0]), ("x.cpp".into(), Some(7), Some(1)));
        assert_eq!(d[0].tool, None);
    }

    #[test]
    fn linker_library_and_undefined_references() {
        // Captured from GNU ld 2.38, English and zh_CN.
        let log = "/usr/bin/ld: cannot find -lnosuchlib: No such file or directory\n\
/usr/bin/ld: 找不到 -lother: 没有那个文件或目录\n\
collect2: error: ld returned 1 exit status\n\
/usr/bin/ld: /tmp/cc67jV4b.o: in function `main':\n\
ld.cpp:(.text+0x9): undefined reference to `foo()'\n\
/usr/bin/ld: /tmp/ccmm7KAF.o: in function `main':\n\
/tmp/dg/ld.cpp:2: undefined reference to `foo()'\n\
collect2: error: ld returned 1 exit status\n\
ld.lld: error: unable to find library -lbaz\n\
ld.lld: error: undefined symbol: bar(int)\n\
>>> referenced by main.cpp:3\n\
>>>               main.o:(main)\n";
        let d = parse_cpp(log);
        let codes: Vec<_> = d.iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                Code::LibraryNotFound,
                Code::LibraryNotFound,
                Code::UndefinedReference,
                Code::UndefinedReference,
                Code::LibraryNotFound,
                Code::UndefinedReference
            ]
        );
        let libs: Vec<_> = [&d[0], &d[1], &d[4]]
            .iter()
            .map(|d| d.explanation.args()[0].1.clone())
            .collect();
        assert_eq!(libs, ["nosuchlib", "other", "baz"]);
        assert_eq!(d[0].tool.as_deref(), Some("ld"));
        assert_eq!(d[2].explanation.args()[0].1, "foo()");
        assert_eq!(loc(&d[2]), ("ld.cpp".into(), None, None));
        assert!(d[2].related[0].raw.contains("in function `main'"));
        assert_eq!(loc(&d[3]), ("/tmp/dg/ld.cpp".into(), Some(2), None));
        assert_eq!(d[5].explanation.args()[0].1, "bar(int)");
        assert_eq!(d[5].tool.as_deref(), Some("ld.lld"));
        assert_eq!(d[5].raw.lines().count(), 3);
        assert!(d.iter().all(|d| d.category == Category::Linking));
    }

    #[test]
    fn cmake_find_package_config_and_module_failures() {
        // Captured from CMake 3.22.
        let log = r#"CMake Error at CMakeLists.txt:3 (find_package):
  By not providing "Findnosuchpkg.cmake" in CMAKE_MODULE_PATH this project
  has asked CMake to find a package configuration file provided by
  "nosuchpkg", but CMake did not find one.

  Could not find a package configuration file provided by "nosuchpkg" with
  any of the following names:

    nosuchpkgConfig.cmake
    nosuchpkg-config.cmake


-- Configuring incomplete, errors occurred!
CMake Error at /usr/share/cmake-3.22/Modules/FindPackageHandleStandardArgs.cmake:230 (message):
  Could NOT find Boost: Found unsuitable version "1.74.0", but required is at
  least "99" (found /usr/include, )
Call Stack (most recent call first):
  /usr/share/cmake-3.22/Modules/FindBoost.cmake:2360 (find_package_handle_standard_args)
  CMakeLists.txt:3 (find_package)


CMake Error: The source directory "/x" does not exist.
"#;
        let d = parse_cpp(log);
        assert_eq!(d.len(), 3);
        assert_eq!(
            (d[0].code, d[0].category),
            (Code::PackageNotFound, Category::PackageDiscovery)
        );
        assert_eq!(loc(&d[0]), ("CMakeLists.txt".into(), Some(3), None));
        assert_eq!(d[0].explanation.args()[0].1, "nosuchpkg");
        assert!(
            d[0].raw.ends_with("nosuchpkg-config.cmake"),
            "{:?}",
            d[0].raw
        );
        assert_eq!(d[1].code, Code::PackageNotFound);
        assert_eq!(d[1].explanation.args()[0].1, "Boost");
        assert!(d[1].raw.contains("CMakeLists.txt:3 (find_package)"));
        assert_eq!(d[2].code, Code::Unknown);
        assert_eq!(d[2].tool.as_deref(), Some("cmake"));
        assert!(d[2].location.is_none());
    }

    #[test]
    fn successful_build_output_yields_nothing() {
        let log = "Starting >>> demo\n[ 50%] Building CXX object CMakeFiles/demo.dir/src/main.cpp.o\n\
src/main.cpp:3:7: warning: unused variable ‘x’ [-Wunused-variable]\n    3 |   int x;\n      |       ^\n\
[100%] Linking CXX executable demo\nFinished <<< demo [2.1s]\nSummary: 1 package finished [2.3s]\n";
        assert!(parse_cpp(log).is_empty());
        assert!(parse_cpp("").is_empty());
        assert!(parse_cpp(
            "random: text: here\n\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\\n"
        )
        .is_empty());
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        assert_eq!(
            strip_ansi("\u{1b}[01;31m\u{1b}[Kerror\u{1b}[m \u{1b}]8;;u\u{7}x\u{1b}]8;;\u{1b}\\"),
            "error x"
        );
    }
}
