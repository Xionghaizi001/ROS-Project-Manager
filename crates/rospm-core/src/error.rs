use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest {path}: {source}")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("failed to parse lock file {path}: {source}")]
    LockParse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("failed to serialize {what}: {source}")]
    Serialize {
        what: &'static str,
        #[source]
        source: Box<toml::ser::Error>,
    },

    #[error("unsupported schema_version {found} (this rospm supports {supported})")]
    SchemaVersion { found: u32, supported: u32 },

    #[error("manifest validation failed:\n{}", format_list(.0))]
    Validation(Vec<String>),

    #[error("unknown workspace `{0}`")]
    UnknownWorkspace(String),

    #[error("{feature} is not implemented yet (planned for roadmap {phase})")]
    NotImplemented {
        feature: &'static str,
        phase: &'static str,
    },

    #[error("command failed: {command} (exit status {status})")]
    CommandFailed { command: String, status: String },
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

fn format_list(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("  - {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub type Result<T> = std::result::Result<T, Error>;
