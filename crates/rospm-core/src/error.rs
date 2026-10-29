use std::fmt;
use std::path::PathBuf;

use crate::message::Message;
use crate::msg;

#[derive(Debug)]
pub enum Error {
    /// Cooperative stop at a safe boundary; completed effects are retained.
    Cancelled,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    ManifestParse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },

    LockParse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },

    PixiParse {
        source: Box<toml::de::Error>,
    },

    ReposParse {
        path: PathBuf,
        source: Box<serde_yaml::Error>,
    },

    PackageXml {
        path: PathBuf,
        message: Message,
    },

    Serialize {
        what: &'static str,
        source: Box<toml::ser::Error>,
    },

    SchemaVersion {
        found: u32,
        supported: u32,
    },

    Validation(Vec<Message>),

    UnknownWorkspace(String),

    NotImplemented {
        feature: &'static str,
        phase: &'static str,
    },

    Host(Message),

    CommandFailed {
        command: String,
        status: String,
    },
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    /// The user-facing description (spec §4.2). `Display` renders this; the
    /// CLI prints it in the user's language.
    pub fn message(&self) -> Message {
        match self {
            Error::Cancelled => msg!("error.cancelled"),
            Error::Io { path, source } => {
                msg!("error.io", path = path.display(), source = source)
            }
            Error::ManifestParse { path, source } => msg!(
                "error.manifest_parse",
                path = path.display(),
                source = source
            ),
            Error::LockParse { path, source } => {
                msg!("error.lock_parse", path = path.display(), source = source)
            }
            Error::PixiParse { source } => msg!("pixi.parse", source = source),
            Error::ReposParse { path, source } => {
                msg!("error.repos_parse", path = path.display(), source = source)
            }
            Error::PackageXml { path, message } => {
                msg!(
                    "error.package_xml",
                    path = path.display(),
                    message = message
                )
            }
            Error::Serialize { what, source } => {
                msg!("error.serialize", what = what, source = source)
            }
            Error::SchemaVersion { found, supported } => {
                msg!("error.schema_version", found = found, supported = supported)
            }
            Error::Validation(list) => msg!("error.validation", list = format_list(list)),
            Error::UnknownWorkspace(name) => msg!("error.unknown_workspace", name = name),
            Error::NotImplemented { feature, phase } => {
                msg!("error.not_implemented", feature = feature, phase = phase)
            }
            Error::Host(message) => message.clone(),
            Error::CommandFailed { command, status } => {
                msg!("error.command_failed", command = command, status = status)
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::ManifestParse { source, .. } | Error::LockParse { source, .. } => {
                Some(source.as_ref())
            }
            Error::PixiParse { source } => Some(source.as_ref()),
            Error::ReposParse { source, .. } => Some(source.as_ref()),
            Error::Serialize { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

fn format_list(items: &[Message]) -> String {
    items
        .iter()
        .map(|i| format!("  - {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub type Result<T> = std::result::Result<T, Error>;
