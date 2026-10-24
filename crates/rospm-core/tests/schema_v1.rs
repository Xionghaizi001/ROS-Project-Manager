//! Frozen on-disk contract. Fixtures are synthetic, never installation inputs.
use std::path::Path;

use rospm_core::{lock::Lock, manifest::Manifest, Error};
use toml::Value;

const MINIMAL: &str = include_str!("fixtures/schema-v1/minimal.toml");
const NATIVE: &str = include_str!("fixtures/schema-v1/native.toml");
const LOCK: &str = include_str!("fixtures/schema-v1/native.lock");

fn manifest(text: &str) -> rospm_core::Result<Manifest> {
    Manifest::parse(text, Path::new("rospm.toml"))
}

fn lock(text: &str) -> rospm_core::Result<Lock> {
    Lock::parse(text, Path::new("rospm.lock"))
}

fn value(text: &str) -> Value {
    toml::from_str(text).unwrap()
}

#[test]
fn full_fixtures_preserve_every_field_and_hash_exact_text() {
    let m = manifest(NATIVE).unwrap();
    assert_eq!(value(&m.to_toml().unwrap()), value(NATIVE));
    assert_eq!(m.build_order().unwrap(), ["deps", "main"]);
    let l = lock(LOCK).unwrap();
    assert_eq!(value(&l.to_toml().unwrap()), value(LOCK));
    assert_eq!(lock(&l.to_toml().unwrap()).unwrap(), l);
    assert!(l.matches_manifest(NATIVE));
    for changed in [
        format!("{NATIVE}\n"),
        format!("# comment\n{NATIVE}"),
        NATIVE.replace('\n', "\r\n"),
    ] {
        assert!(!l.matches_manifest(&changed));
    }
    let serialized = m.to_toml().unwrap();
    assert_eq!(
        manifest(&serialized).unwrap().to_toml().unwrap(),
        serialized
    );
}

#[test]
fn omitted_fields_have_frozen_defaults() {
    let m = manifest(MINIMAL).unwrap();
    let v = value(&m.to_toml().unwrap());
    assert_eq!(
        v,
        value(
            &format!(
                r#"{MINIMAL}
[workspaces.main.colcon]
mixins = []
build_args = []
[dependencies]
apt = []
pip = []
[dependencies.rosdep]
enabled = true
skip_keys = []
extra_sources = []
[target]
[env]
[editor]
vscode = false
clion = false
devcontainer = false
[export]
compression = "none"
sign = false
"#
            )
            .replace("path = \"ws\"", "path = \"ws\"\nunderlays = []")
        )
    );
    assert_eq!(m.ros_base_package(), "ros-humble-ros-base");
    let minimal_lock = r#"schema_version = 1
generated_at = "2026-10-25T00:00:00Z"
manifest_sha256 = "sha256:placeholder"
[provider]
kind = "native"
[provider.native]
ubuntu = "22.04"
"#;
    let l = lock(minimal_lock).unwrap();
    assert!(l.sources.is_empty() && l.apt.is_empty() && l.pip.is_empty() && l.conda.is_empty());
    let native = l.provider.native.as_ref().unwrap();
    assert!(native.ros_snapshot.is_none() && native.ubuntu_snapshot.is_none());
    assert_eq!(lock(&l.to_toml().unwrap()).unwrap(), l);
}

// Walk every fixed table in a fixture, including array elements. The three
// manifest maps have user-defined keys; their values still obey their types.
fn unknown_fields(v: &Value, path: &str, results: &mut Vec<(String, Value)>) {
    match v {
        Value::Table(table) => {
            if !matches!(
                path,
                "workspaces" | "target" | "env" | "target.linux-aarch64.replace"
            ) {
                let mut bad = v.clone();
                bad.as_table_mut()
                    .unwrap()
                    .insert("unknown_v1_field".into(), Value::Boolean(true));
                results.push((path.into(), bad));
            }
            for (key, child) in table {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let mut mutations = Vec::new();
                unknown_fields(child, &child_path, &mut mutations);
                for (location, mutation) in mutations {
                    let mut bad = v.clone();
                    bad.as_table_mut().unwrap().insert(key.clone(), mutation);
                    results.push((location, bad));
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let mut mutations = Vec::new();
                unknown_fields(item, path, &mut mutations);
                for (location, mutation) in mutations {
                    let mut bad = v.clone();
                    bad.as_array_mut().unwrap()[i] = mutation;
                    results.push((location, bad));
                }
            }
        }
        _ => {}
    }
}

#[test]
fn every_fixed_table_rejects_unknown_fields() {
    let mut manifests = vec![NATIVE.to_owned()];
    for table in ["name = \"robot\"", "prefix = \".conda/robot\""] {
        manifests.push(format!("{MINIMAL}\n[build.conda]\n{table}\n"));
    }
    for (provider, table) in [("pixi", "manifest = \"pixi.toml\""), ("docker", "base_image = \"ros:jazzy\"\ndockerfile = \"Dockerfile\"\nplatforms = [\"linux/arm64\"]")] {
        manifests.push(format!("{}\n[environment.{provider}]\n{table}\n", MINIMAL.replace("provider = \"native\"", &format!("provider = \"{provider}\""))));
    }
    for text in manifests {
        manifest(&text).unwrap();
        let mut cases = Vec::new();
        unknown_fields(&value(&text), "", &mut cases);
        for (path, bad) in cases {
            assert!(
                matches!(
                    manifest(&toml::to_string(&bad).unwrap()),
                    Err(Error::ManifestParse { .. })
                ),
                "{path}"
            );
        }
    }
    let mut locks = vec![LOCK.to_owned()];
    for (provider, table) in [
        (
            "pixi",
            "manifest = \"pixi.toml\"\nlock = \"pixi.lock\"\nlock_sha256 = \"sha256:test\"",
        ),
        (
            "docker",
            "base_image = \"ros:jazzy\"\ndigest = \"sha256:test\"",
        ),
    ] {
        locks.push(format!("schema_version = 1\ngenerated_at = \"2026-10-25T00:00:00Z\"\nmanifest_sha256 = \"sha256:test\"\n[provider]\nkind = \"{provider}\"\n[provider.{provider}]\n{table}\n"));
    }
    for text in locks {
        let l = lock(&text).unwrap();
        assert_eq!(lock(&l.to_toml().unwrap()).unwrap(), l);
        let mut cases = Vec::new();
        unknown_fields(&value(&text), "", &mut cases);
        for (path, bad) in cases {
            assert!(
                matches!(
                    lock(&toml::to_string(&bad).unwrap()),
                    Err(Error::LockParse { .. })
                ),
                "{path}"
            );
        }
    }
}

#[test]
fn versions_types_required_fields_and_platform_names_are_strict() {
    for version in ["0", "2", "4294967295", "-1", "4294967296", "\"1\"", "1.0"] {
        assert!(manifest(&MINIMAL.replacen(
            "schema_version = 1",
            &format!("schema_version = {version}"),
            1
        ))
        .is_err());
        assert!(lock(&LOCK.replacen(
            "schema_version = 1",
            &format!("schema_version = {version}"),
            1
        ))
        .is_err());
    }
    for platform in [
        "amd64",
        "arm64",
        "x86_64",
        "aarch64",
        "linux/amd64",
        "linux-64",
    ] {
        assert!(manifest(&MINIMAL.replace("linux-x86_64", platform)).is_err());
        assert!(lock(&LOCK.replace("linux-x86_64", platform)).is_err());
    }
    for path in [
        vec!["schema_version"],
        vec!["project"],
        vec!["environment"],
        vec!["workspaces"],
        vec!["project", "name"],
        vec!["project", "ros_distro"],
        vec!["project", "platforms"],
        vec!["environment", "provider"],
        vec!["workspaces", "main", "path"],
    ] {
        let mut v = value(MINIMAL);
        let mut table = &mut v;
        for key in &path[..path.len() - 1] {
            table = table.get_mut(key).unwrap();
        }
        table.as_table_mut().unwrap().remove(*path.last().unwrap());
        assert!(manifest(&toml::to_string(&v).unwrap()).is_err(), "{path:?}");
    }
    for field in [
        "schema_version",
        "generated_at",
        "manifest_sha256",
        "provider",
    ] {
        let mut v = value(LOCK);
        v.as_table_mut().unwrap().remove(field);
        assert!(lock(&toml::to_string(&v).unwrap()).is_err(), "{field}");
    }
}
