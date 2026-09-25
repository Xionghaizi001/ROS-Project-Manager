//! Helpers shared by the CLI integration tests (`tests/*.rs`). Each test
//! file declares `mod common;` and uses what it needs.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The binary with the language pinned to English, whatever the developer's
/// locale is (spec §4.2: `C` → en).
pub fn rospm() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rospm"));
    for k in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "ROSPM_LANG"] {
        c.env_remove(k);
    }
    c.env("LANG", "C");
    c
}

/// Same, pinned to Chinese through the locale alone.
pub fn rospm_zh() -> Command {
    let mut c = rospm();
    c.env("LANG", "zh_CN.UTF-8");
    c
}

pub fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rospm-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("ws/src/my_pkg")).unwrap();
    fs::write(
        dir.join("ws/src/my_pkg/package.xml"),
        "<package format=\"3\"><name>my_pkg</name><depend>rclcpp</depend></package>",
    )
    .unwrap();
    fs::write(
        dir.join("main.repos"),
        "repositories:\n  ros2/demos:\n    type: git\n    url: https://github.com/ros2/demos.git\n    version: jazzy\n",
    )
    .unwrap();
    dir
}

pub fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON ({e}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
}
