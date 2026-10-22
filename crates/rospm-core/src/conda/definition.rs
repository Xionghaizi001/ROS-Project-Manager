//! The normalized environment definition: the manifest's Python version and
//! channels merged with the optional `environment_file`.

use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::DEFINITION;
use crate::error::{Error, Result};
use crate::lock::sha256_hex;
use crate::manifest::ManagedConda;
use crate::msg;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentFile {
    // Input names and prefixes never decide where a managed environment lives.
    #[serde(default, rename = "name")]
    _name: Option<String>,
    #[serde(default, rename = "prefix")]
    _prefix: Option<PathBuf>,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    dependencies: Vec<serde_yaml::Value>,
    #[serde(default, deserialize_with = "scalar_variables")]
    variables: IndexMap<String, String>,
}

/// `ROS_DOMAIN_ID: 0` and `USE_SIM: true` are as common as quoted values:
/// numbers and booleans become their YAML text, anything else names the key.
fn scalar_variables<'de, D>(
    deserializer: D,
) -> std::result::Result<IndexMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = IndexMap::<String, serde_yaml::Value>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|(key, value)| {
            let text = match value {
                serde_yaml::Value::String(s) => s,
                serde_yaml::Value::Number(n) => n.to_string(),
                serde_yaml::Value::Bool(b) => b.to_string(),
                _ => {
                    return Err(serde::de::Error::custom(format!(
                        "variables.{key} must be a string, number or boolean"
                    )))
                }
            };
            Ok((key, text))
        })
        .collect()
}

#[derive(Serialize)]
struct GeneratedEnvironment {
    channels: Vec<String>,
    dependencies: Vec<serde_yaml::Value>,
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    variables: IndexMap<String, String>,
}

/// Digest of the normalized definition, as recorded in a lock record.
pub fn definition_sha256(managed: &ManagedConda, root: &Path) -> Result<String> {
    Ok(sha256_hex(&environment_yaml(managed, root)?.contents))
}

/// The normalized definition and what lock replay needs to know about it.
pub(super) struct Definition {
    pub(super) contents: String,
    /// Conda match specs; `python` included.
    pub(super) conda: Vec<String>,
    /// Requirements of the `pip:` entry.
    pub(super) pip: Vec<String>,
    pub(super) variables: IndexMap<String, String>,
}

pub(super) fn environment_yaml(managed: &ManagedConda, root: &Path) -> Result<Definition> {
    let mut input = if let Some(file) = &managed.environment_file {
        let path = root.join(file);
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        serde_yaml::from_str::<EnvironmentFile>(&text).map_err(|e| {
            Error::Host(msg!(
                "conda.definition_invalid",
                path = path.display(),
                error = e
            ))
        })?
    } else {
        EnvironmentFile::default()
    };
    let channels: Vec<_> = managed
        .channels
        .iter()
        .filter(|c| *c != "nodefaults")
        .cloned()
        .collect();
    let file_channels: Vec<_> = input
        .channels
        .iter()
        .filter(|c| *c != "nodefaults")
        .cloned()
        .collect();
    if !file_channels.is_empty() && file_channels != channels {
        return Err(Error::Host(msg!("conda.channels_conflict")));
    }
    let python = format!("python={}", managed.python);
    for dependency in &mut input.dependencies {
        if let Some(spec) = dependency.as_str() {
            if spec_name(spec) == "python" {
                if spec != "python" && spec != python {
                    return Err(Error::Host(msg!(
                        "conda.python_conflict",
                        expected = python,
                        found = spec
                    )));
                }
                *dependency = serde_yaml::Value::String(python.clone());
            }
        }
    }
    if !input
        .dependencies
        .iter()
        .any(|d| d.as_str() == Some(&python))
    {
        input
            .dependencies
            .insert(0, serde_yaml::Value::String(python));
    }
    let mut channels = channels;
    channels.push("nodefaults".into());
    let conda = input
        .dependencies
        .iter()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    let pip = input
        .dependencies
        .iter()
        .filter_map(|d| d.get("pip")?.as_sequence())
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_string))
        .collect();
    let contents = serde_yaml::to_string(&GeneratedEnvironment {
        channels,
        dependencies: input.dependencies,
        variables: input.variables.clone(),
    })
    .map_err(|e| {
        Error::Host(msg!(
            "conda.definition_invalid",
            path = DEFINITION,
            error = e
        ))
    })?;
    Ok(Definition {
        contents,
        conda,
        pip,
        variables: input.variables,
    })
}

/// The package name of a Conda match spec, without channel.
pub(super) fn spec_name(spec: &str) -> &str {
    let package = spec
        .trim()
        .split(['=', '<', '>', '!', '~', ' ', '['])
        .next()
        .unwrap_or("");
    package.rsplit("::").next().unwrap_or(package)
}
