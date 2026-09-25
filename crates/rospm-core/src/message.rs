//! Locale-independent user-facing text (spec §4.2, decision #16).
//!
//! The core library never contains a translated sentence. Everything a user
//! will read — plan notes, `check` report cells, error descriptions,
//! `init --from-env` notes — is a [`Message`]: either verbatim technical text
//! (a path, a package name, a version) or a *message key* plus named
//! arguments. The CLI installs a [`Renderer`] once at start-up that turns keys
//! into the user's language; until one is installed (unit tests, other
//! consumers of the library) a message displays in its developer form
//! `key(name=value, …)`, which is stable and greppable but not meant for
//! end users.
//!
//! Placeholders in the translated text use the `%{name}` syntax so the same
//! resource files work with `rust-i18n`.

use std::fmt;
use std::sync::OnceLock;

use serde::{Serialize, Serializer};

/// One piece of user-facing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Verbatim text that must not be translated (paths, package names,
    /// versions, URLs, output of other tools).
    Text(String),
    /// A translation key with named arguments, rendered by the CLI.
    Key {
        key: &'static str,
        args: Vec<(&'static str, String)>,
    },
}

/// Turns a [`Message::Key`] into text for the user. Installed by the CLI.
pub type Renderer = Box<dyn Fn(&Message) -> String + Send + Sync>;

static RENDERER: OnceLock<Renderer> = OnceLock::new();

/// Install the process-wide renderer. Only the first call takes effect; the
/// return value tells the caller whether it did.
pub fn set_renderer(f: impl Fn(&Message) -> String + Send + Sync + 'static) -> bool {
    RENDERER.set(Box::new(f)).is_ok()
}

impl Message {
    /// Verbatim text, shown as-is in every language.
    pub fn text(s: impl Into<String>) -> Message {
        Message::Text(s.into())
    }

    /// A message key without arguments. Prefer the [`msg!`](crate::msg) macro.
    pub fn key(key: &'static str) -> Message {
        Message::Key {
            key,
            args: Vec::new(),
        }
    }

    /// Add a named argument (`%{name}` in the translated text). Arguments are
    /// stringified immediately so the message owns everything it needs.
    pub fn arg(mut self, name: &'static str, value: impl ToString) -> Message {
        if let Message::Key { args, .. } = &mut self {
            args.push((name, value.to_string()));
        }
        self
    }

    /// The translation key, `None` for verbatim text.
    pub fn key_name(&self) -> Option<&'static str> {
        match self {
            Message::Text(_) => None,
            Message::Key { key, .. } => Some(key),
        }
    }

    pub fn args(&self) -> &[(&'static str, String)] {
        match self {
            Message::Text(_) => &[],
            Message::Key { args, .. } => args,
        }
    }

    /// Whether this message is a key starting with `prefix` (keys are
    /// dot-separated namespaces, e.g. `init.ros_base.`).
    pub fn has_key_prefix(&self, prefix: &str) -> bool {
        self.key_name().is_some_and(|k| k.starts_with(prefix))
    }

    /// `key(name=value, …)`: what a message looks like when no renderer is
    /// installed. Never shown to end users by the CLI.
    pub fn developer_form(&self) -> String {
        match self {
            Message::Text(s) => s.clone(),
            Message::Key { key, args } => {
                if args.is_empty() {
                    key.to_string()
                } else {
                    let args: Vec<String> = args.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    format!("{key}({})", args.join(", "))
                }
            }
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Text(s) => f.write_str(s),
            Message::Key { .. } => match RENDERER.get() {
                Some(render) => f.write_str(&render(self)),
                None => f.write_str(&self.developer_form()),
            },
        }
    }
}

/// Serialised as the rendered string, so `--json` free-text fields follow
/// the user's language (spec §4.2) while enum fields stay stable.
impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<String> for Message {
    fn from(s: String) -> Message {
        Message::Text(s)
    }
}

impl From<&String> for Message {
    fn from(s: &String) -> Message {
        Message::Text(s.clone())
    }
}

impl From<&std::path::Path> for Message {
    fn from(p: &std::path::Path) -> Message {
        Message::Text(p.display().to_string())
    }
}

impl From<std::path::PathBuf> for Message {
    fn from(p: std::path::PathBuf) -> Message {
        Message::Text(p.display().to_string())
    }
}

/// Build a [`Message::Key`]: `msg!("plan.dry_run")`,
/// `msg!("native.apply.wrong_ubuntu", ubuntu = "24.04", have = os)`.
#[macro_export]
macro_rules! msg {
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::message::Message::key($key)$(.arg(stringify!($name), &$value))*
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn developer_form_is_key_and_args() {
        let m = msg!("a.b", n = 2, path = "/x");
        assert_eq!(m.key_name(), Some("a.b"));
        assert_eq!(m.developer_form(), "a.b(n=2, path=/x)");
        assert_eq!(msg!("a.c").developer_form(), "a.c");
        assert_eq!(Message::text("verbatim").to_string(), "verbatim");
        assert!(m.has_key_prefix("a."));
        assert!(!Message::text("a.b").has_key_prefix("a."));
    }

    #[test]
    fn serializes_as_a_string() {
        let v = serde_json::to_value(msg!("k", x = 1)).unwrap();
        assert!(v.is_string());
    }
}
