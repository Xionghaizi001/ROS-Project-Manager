//! Real compiler/colcon acceptance, opt-in because it needs ROS and both compilers.
//! See docs/p36-diagnostics-validation.md for prerequisites and evidence layout.
use std::{path::PathBuf, process::Command, time::SystemTime};

#[test]
#[ignore = "requires ROS Humble, colcon, CMake, GCC and Clang; see docs/p36-diagnostics-validation.md"]
fn real_cpp_python_failures_through_cli() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = std::env::temp_dir().join(format!(
        "rospm-p36-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg(repo.join("scripts/p36-diagnostics-check.py"))
        .arg("--root")
        .arg(&root)
        .args(["--rospm", env!("CARGO_BIN_EXE_rospm")]);
    if let Some(clang) = std::env::var_os("P36_CLANGXX") {
        command.arg("--clang").arg(clang);
    }
    if let Some(gcc) = std::env::var_os("P36_GXX") {
        command.arg("--gcc").arg(gcc);
    }
    let status = command.status().expect("start acceptance script");
    assert!(
        status.success(),
        "acceptance failed; evidence: {}",
        root.display()
    );
}
