//! End-to-end checks of the `rospm` binary for the P1 `init` / `check` paths.
//! They only touch a temporary directory and never modify the system.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::*;

#[test]
fn init_from_env_writes_a_valid_manifest_that_check_accepts() {
    let dir = fixture("init");
    let manifest = dir.join("rospm.toml");

    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--from-env", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["ok"], true);
    let ws = v["draft"]["workspaces"].as_array().unwrap();
    assert_eq!(ws.len(), 1, "{v:#}");
    assert_eq!(ws[0]["name"], "main");
    assert_eq!(ws[0]["path"], "ws");
    assert_eq!(ws[0]["sources"], "main.repos");
    assert!(manifest.is_file());
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(
        text.contains("TODO"),
        "a draft without any TODO is suspicious:\n{text}"
    );
    assert!(text.contains("#   rclcpp"), "{text}");

    // Refuses to overwrite without --force, like the template path.
    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--from-env"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));

    // The drafted manifest passes `check`, and the host is probed.
    let out = rospm()
        .current_dir(&dir)
        .args(["check", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["build_order"], serde_json::json!(["main"]));
    let env = &v["environment"];
    assert_eq!(env["probed"], true);
    let items = env["items"].as_array().unwrap();
    let demos = items
        .iter()
        .find(|i| i["subject"] == "main: src/ros2/demos")
        .expect("repo from main.repos is reported");
    assert_eq!(demos["status"], "missing");
    assert_eq!(demos["actual"], "not imported");
    let pkgs = items.iter().find(|i| i["subject"] == "main: src/").unwrap();
    assert_eq!(pkgs["actual"], "1 package");
    assert_eq!(env["ready"], false);

    // --strict turns "not ready" into a non-zero exit; without it check is advisory.
    let out = rospm()
        .current_dir(&dir)
        .args(["check", "--strict"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("not ready"));
}

#[test]
fn init_base_picks_the_metapackage() {
    let dir = fixture("base");
    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--base", "desktop"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    assert!(
        text.contains("\nros_base = \"ros-jazzy-desktop\""),
        "{text}"
    );

    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--force", "--base", "ros-base"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    assert!(
        text.contains("\nros_base = \"ros-jazzy-ros-base\""),
        "{text}"
    );

    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--force", "--base", "desktop-full"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "unknown flavor must be rejected by clap"
    );
}

#[test]
fn uninstall_refuses_without_a_terminal_and_dry_run_only_prints() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/minimal/rospm.toml");
    let manifest = example.to_string_lossy().into_owned();
    // Planning needs dpkg-query; on hosts without it the command must fail
    // cleanly rather than guess.
    let has_dpkg = which::which("dpkg-query").is_ok();

    let out = rospm()
        .args(["--manifest", &manifest, "uninstall", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if has_dpkg {
        assert!(out.status.success(), "{stderr}");
        assert!(stdout.contains("# plan: uninstall ROS jazzy"), "{stdout}");
        assert!(stdout.contains("dry-run: nothing executed"), "{stdout}");
    } else {
        assert!(!out.status.success());
        assert!(stderr.contains("dpkg-query"), "{stderr}");
    }

    // No --yes and stdin is a pipe: nothing may be executed.
    let out = rospm()
        .args(["--manifest", &manifest, "uninstall"])
        .stdin(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.contains("nothing to do") {
        // Host without ROS: an empty plan needs no confirmation.
        assert!(out.status.success(), "{stderr}");
    } else {
        assert!(!out.status.success());
        assert!(
            stderr.contains("--yes") || stderr.contains("dpkg-query"),
            "stdout: {stdout}\nstderr: {stderr}"
        );
    }
    assert!(!stdout.contains("==> ["), "a step was executed:\n{stdout}");
}

#[test]
fn check_for_a_foreign_target_skips_probing() {
    let dir = fixture("target");
    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--from-env"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = rospm()
        .current_dir(&dir)
        .args(["check", "--target", "linux-aarch64", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["environment"]["probed"], false);
    assert!(v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("probing skipped")));
}

#[test]
fn check_on_the_bundled_example_stays_green_without_ros() {
    // Mirrors the CI smoke step: `check` must succeed on a host without ROS.
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/minimal/rospm.toml");
    let out = rospm()
        .args(["--manifest", &example.to_string_lossy(), "check", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["environment"]["probed"], true);
}

#[test]
fn apply_dry_run_plans_with_a_mirror_without_probing_the_network() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/minimal/rospm.toml");
    let manifest = example.to_string_lossy().into_owned();

    // Default: official sources, no probe (dry-run must stay offline-safe).
    let out = rospm()
        .args(["--manifest", &manifest, "apply", "--dry-run"])
        .env_remove("ROSPM_MIRROR")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        // Host on another Ubuntu release: plan_apply refuses before anything else.
        assert!(
            stderr.contains("rospm does not install or upgrade Ubuntu"),
            "{stderr}"
        );
        return;
    }
    assert!(
        stdout.contains("# network: not probed (--dry-run)"),
        "{stdout}"
    );
    assert!(!stdout.contains("network check ("), "{stdout}");

    // Explicit mirror via flag and via ROSPM_MIRROR.
    let cases: [(&[&str], Option<&str>); 2] = [
        (&["apply", "--dry-run", "--mirror", "tuna"], None),
        (&["apply", "--dry-run"], Some("tuna")),
    ];
    for (args, env) in cases {
        let mut cmd = rospm();
        cmd.args(["--manifest", &manifest]).args(args);
        match env {
            Some(v) => cmd.env("ROSPM_MIRROR", v),
            None => cmd.env_remove("ROSPM_MIRROR"),
        };
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("planning with the tuna sources"),
            "{stdout}"
        );
        assert!(
            stdout.contains("https://mirrors.tuna.tsinghua.edu.cn/ros2/ubuntu noble main"),
            "{stdout}"
        );
        assert!(stdout.contains("verify fingerprint C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654"));
        assert!(stdout.contains(
            "ROSDISTRO_INDEX_URL=https://mirrors.tuna.tsinghua.edu.cn/rosdistro/index-v4.yaml"
        ));
        assert!(stdout.contains("dry-run: nothing executed"), "{stdout}");
    }

    // Unknown names are rejected up front.
    let out = rospm()
        .args([
            "--manifest",
            &manifest,
            "apply",
            "--dry-run",
            "--mirror",
            "sjtu",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = rospm()
        .args(["--manifest", &manifest, "apply", "--dry-run"])
        .env("ROSPM_MIRROR", "sjtu")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ROSPM_MIRROR"));
}

// ---------------------------------------------------------------- i18n (P1.5)

#[test]
fn chinese_locale_localises_help_check_apply_and_errors() {
    let dir = fixture("zh");

    let out = rospm_zh().arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("用法:") && help.contains("命令:") && help.contains("选项:"),
        "{help}"
    );
    assert!(!help.contains("Print help"), "{help}");
    let out = rospm_zh().args(["help", "build"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("参数:"));

    // init --from-env: detected / TODO lines and the drafted file's comments.
    let out = rospm_zh()
        .current_dir(&dir)
        .args(["init", "--from-env"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("已探测") && stdout.contains("TODO "),
        "{stdout}"
    );
    let text = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    assert!(text.contains("TODO") && has_cjk(&text), "{text}");

    // check: Chinese table, same exit codes as English.
    let out = rospm_zh().current_dir(&dir).arg("check").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("清单有效") && stdout.contains("未导入"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("not ready") && !stdout.contains("MISSING"),
        "{stdout}"
    );
    let out = rospm_zh()
        .current_dir(&dir)
        .args(["check", "--strict"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("未就绪"));

    // --json: key names and enum values stay English, free text follows the language.
    let out = rospm_zh()
        .current_dir(&dir)
        .args(["check", "--json"])
        .output()
        .unwrap();
    let v = json(&out);
    let items = v["environment"]["items"].as_array().unwrap();
    let demos = items
        .iter()
        .find(|i| i["subject"] == "main: src/ros2/demos")
        .unwrap();
    assert_eq!(demos["status"], "missing");
    assert_eq!(demos["actual"], "未导入");

    // apply --dry-run: title, notes and the dry-run trailer are Chinese; commands are not.
    let out = rospm_zh()
        .current_dir(&dir)
        .args(["apply", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("# 计划：apply") && stdout.contains("未执行任何步骤"),
        "{stdout}"
    );
    assert!(
        stdout.contains("sudo apt-get install -y ros-humble-ros-base"),
        "{stdout}"
    );

    // Errors, plain and --json.
    let out = rospm_zh()
        .args(["check", "--manifest", "/nonexistent"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("错误：") && stderr.contains("读写出错"),
        "{stderr}"
    );
    let out = rospm_zh()
        .args(["--manifest", "/nonexistent", "--json", "export"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v = json(&out);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("尚未实现"), "{v}");
}

#[test]
fn language_overrides_follow_the_documented_order() {
    let manifest = "/nonexistent";
    let run = |c: &mut Command| {
        let out = c.args(["check", "--manifest", manifest]).output().unwrap();
        assert!(!out.status.success());
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    // --lang beats ROSPM_LANG beats the locale.
    assert!(run(rospm_zh().arg("--lang=en")).starts_with("error:"));
    assert!(run(rospm().args(["--lang", "zh"])).starts_with("错误："));
    assert!(run(rospm().env("ROSPM_LANG", "zh")).starts_with("错误："));
    assert!(run(rospm_zh().env("ROSPM_LANG", "en")).starts_with("error:"));
    // LANGUAGE is a list: the first usable entry wins over LANG.
    assert!(run(rospm_zh().env("LANGUAGE", "en:zh")).starts_with("error:"));
    assert!(run(rospm().env("LANGUAGE", "zh:en")).starts_with("错误："));
    assert!(run(rospm().env("LC_ALL", "zh_TW")).starts_with("错误："));
    // English output stays English under C.
    assert!(run(&mut rospm()).starts_with("error: I/O error on /nonexistent"));
}
