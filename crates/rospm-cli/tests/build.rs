//! Conda selection and build invocation; no Conda or ROS installation required.
mod common;

use std::fs;
use std::path::Path;

use common::*;

const MINIMAL: &str = include_str!("../../rospm-core/tests/fixtures/minimal.toml");

fn preview(dir: &Path, args: &[&str]) -> String {
    let out = rospm()
        .current_dir(dir)
        .args(["build", "--dry-run"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn conda_cli_overrides_the_entire_manifest_selection_without_writing() {
    let dir = fixture("build-conda-overrides");
    let manifest = dir.join("rospm.toml");
    fs::write(&manifest, MINIMAL).unwrap();
    let plain = preview(&dir, &[]);
    assert!(plain.contains("colcon build"), "{plain}");
    assert!(!plain.contains("conda run"), "{plain}");

    let named = format!("{MINIMAL}\n[build.conda]\nname = 'manifest-env'\n");
    fs::write(&manifest, &named).unwrap();
    assert!(preview(&dir, &[]).contains("conda run --no-capture-output -n manifest-env"));
    let by_prefix = preview(&dir, &["--conda-prefix", "env with spaces"]);
    let cli_prefix = dir.join("env with spaces");
    assert!(
        by_prefix.contains(&format!("-p '{}'", cli_prefix.display())),
        "{by_prefix}"
    );
    assert!(!by_prefix.contains("-n manifest-env"));
    assert_eq!(fs::read_to_string(&manifest).unwrap(), named);

    let prefixed = format!("{MINIMAL}\n[build.conda]\nprefix = '.conda/project env'\n");
    fs::write(&manifest, &prefixed).unwrap();
    // Invoke from a nested directory: manifest and CLI relative paths have
    // deliberately different bases, neither is relative to the workspace.
    let nested = dir.join("nested");
    fs::create_dir(&nested).unwrap();
    let default = preview(&nested, &[]);
    assert!(
        default.contains(&format!(
            "-p '{}'",
            dir.join(".conda/project env").display()
        )),
        "{default}"
    );
    let named_override = preview(&nested, &["--conda-env", "cli-env"]);
    assert!(named_override.contains("-n cli-env"), "{named_override}");
    assert!(!named_override.contains("-p "));
    let relative_override = preview(&nested, &["--conda-prefix", "local-env"]);
    assert!(
        relative_override.contains(&format!("-p {}", nested.join("local-env").display())),
        "{relative_override}"
    );
    let absolute = dir.join("absolute-env");
    assert!(
        preview(&nested, &["--conda-prefix", absolute.to_str().unwrap()])
            .contains(&format!("-p {}", absolute.display()))
    );
    assert_eq!(fs::read_to_string(&manifest).unwrap(), prefixed);
    for path in [
        ".rospm",
        ".conda",
        "env with spaces",
        "nested/local-env",
        "ws/build",
        "ws/install",
        "ws/log",
    ] {
        assert!(!dir.join(path).exists(), "preview created {path}");
    }
}

#[test]
fn conda_flags_are_mutually_exclusive_and_reject_empty_values() {
    let dir = fixture("build-conda-invalid");
    fs::write(dir.join("rospm.toml"), MINIMAL).unwrap();
    for args in [
        vec!["--conda-env", "robot", "--conda-prefix", "env"],
        vec!["--conda-env", ""],
        vec!["--conda-env", "   "],
        vec!["--conda-prefix", ""],
    ] {
        let out = rospm()
            .current_dir(&dir)
            .args(["build", "--dry-run"])
            .args(&args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "accepted {args:?}");
    }
}

#[test]
fn conda_help_and_configuration_errors_are_localized() {
    let dir = fixture("build-conda-help");
    fs::write(
        dir.join("rospm.toml"),
        format!("{MINIMAL}\n[build.conda]\n"),
    )
    .unwrap();
    for (lang, help, error) in [
        (
            "en",
            "overriding build.conda",
            "exactly one of name or prefix",
        ),
        ("zh", "覆盖 build.conda", "必须且只能设置"),
    ] {
        let out = rospm()
            .args(["--lang", lang, "build", "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("--conda-env") && text.contains("--conda-prefix"));
        assert!(text.contains(help), "{text}");
        let out = rospm()
            .current_dir(&dir)
            .args(["--lang", lang, "build", "--dry-run"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let text = String::from_utf8(out.stderr).unwrap();
        assert!(text.contains(error), "{text}");
    }
}

#[test]
fn managed_apply_preview_resolves_inputs_without_writing_and_cli_can_select_external() {
    let platform = rospm_core::platform::Platform::current().expect("supported test platform");
    let subdir = rospm_core::pixi::conda_platform(platform);
    let dir = fixture("managed-conda-preview");
    // Use init's host-matching distro: apply deliberately rejects a Jazzy
    // manifest on an Ubuntu 22.04 development machine.
    let init = rospm().current_dir(&dir).args(["init"]).output().unwrap();
    assert!(init.status.success());
    let base = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    fs::create_dir(dir.join("config")).unwrap();
    fs::write(
        dir.join("config/environment.yml"),
        "name: ignored-name\nprefix: /ignored-prefix\ndependencies: [numpy]\n",
    )
    .unwrap();
    let text = format!("{base}\n[build.conda.managed]\npython = '3.12'\nchannels = ['conda-forge']\nenvironment_file = 'config/environment.yml'\n");
    fs::write(dir.join("rospm.toml"), &text).unwrap();
    let nested = dir.join("nested");
    fs::create_dir(&nested).unwrap();
    let user = dir.join("user");
    for (lang, expected, bootstrap) in [
        (
            "en",
            "prepare project Conda environment",
            "otherwise download Miniforge 26.7.2-0",
        ),
        ("zh", "准备项目专用 Conda 环境", "下载 Miniforge 26.7.2-0"),
    ] {
        let out = rospm()
            .current_dir(&nested)
            .env("PATH", "")
            .env("XDG_DATA_HOME", user.join("data"))
            .env("XDG_CACHE_HOME", user.join("cache"))
            .args(["--lang", lang, "apply", "--dry-run"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let preview = String::from_utf8(out.stdout).unwrap();
        assert!(preview.contains(expected), "{preview}");
        assert!(preview.contains(dir.join("config/environment.yml").to_str().unwrap()));
        assert!(preview.contains(dir.join(".rospm/conda/env").to_str().unwrap()));
        assert!(preview.contains("python=3.12") && preview.contains("numpy"));
        assert!(!preview.contains("/ignored-prefix"));
        // The bootstrap is described in full but nothing is downloaded.
        assert!(preview.contains(bootstrap), "{preview}");
        assert!(
            preview.contains("281b0ac7d550802efc81af633225a5e6116d29ae72f3ab4eae7168c3931a4c05")
                || preview
                    .contains("89b786c8d2c8b0fda7553914c1314ae4ddaa094503802f279377b19ac4463cb2")
        );
        assert!(preview.contains(
            user.join(format!("data/rospm/conda/miniforge3-26.7.2-0-{subdir}"))
                .to_str()
                .unwrap()
        ));
        assert!(preview.contains(user.join("cache/rospm/downloads").to_str().unwrap()));
    }
    assert!(!dir.join(".rospm").exists() && !user.exists());
    for (lang, expected) in [
        ("en", "run `rospm apply` to install Conda"),
        ("zh", "请执行 `rospm apply` 安装 Conda"),
    ] {
        let out = rospm()
            .current_dir(&nested)
            .env("PATH", "")
            .env("XDG_DATA_HOME", user.join("data"))
            .env("XDG_CACHE_HOME", user.join("cache"))
            .args(["--lang", lang, "build"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(error.contains(expected), "{error}");
    }
    assert!(!dir.join(".rospm").exists() && !user.exists());
    assert!(!dir.join("ws/build").exists());
    let build = preview(&nested, &[]);
    assert!(build.contains(&format!(
        "conda run --no-capture-output -p {}",
        dir.join(".rospm/conda/env").display()
    )));
    assert!(!build.contains("conda env create"));
    assert!(preview(&nested, &["--conda-env", "external"]).contains("-n external"));
    // The lock reference alone changes nothing: plain apply still solves the
    // definition; only `apply --locked` replays (tests/apply_locked.rs).
    let locked = text + "lock_file = 'locks/explicit.lock'\n";
    fs::write(dir.join("rospm.toml"), &locked).unwrap();
    let out = rospm()
        .current_dir(&nested)
        .args(["apply", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview = String::from_utf8(out.stdout).unwrap();
    assert!(
        preview.contains("conda env create --yes --prefix"),
        "{preview}"
    );
    assert!(!preview.contains("explicit.lock"), "{preview}");
    assert_eq!(fs::read_to_string(dir.join("rospm.toml")).unwrap(), locked);
    assert!(!dir.join("locks").exists());
    assert!(!dir.join(".rospm").exists());
}

#[cfg(unix)]
#[test]
fn missing_existing_environment_has_a_localized_error_and_never_runs_build() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixture("missing-conda-environment");
    fs::write(
        dir.join("rospm.toml"),
        format!("{MINIMAL}\n[build.conda]\nname = 'absent'\n"),
    )
    .unwrap();
    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    let conda = bin.join("conda");
    fs::write(&conda, "#!/bin/sh\nif [ \"$1\" = env ] && [ \"$2\" = list ]; then\n  printf '{\"envs\":[]}\\n'\n  exit 0\nfi\nexit 99\n").unwrap();
    fs::set_permissions(&conda, fs::Permissions::from_mode(0o755)).unwrap();
    for (lang, expected) in [("en", "was not found"), ("zh", "未找到已有 Conda 环境")] {
        let out = rospm()
            .current_dir(&dir)
            .env("PATH", &bin)
            .args(["--lang", lang, "build"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(
            error.contains(expected) && error.contains("-n absent"),
            "{error}"
        );
        assert!(!error.contains("99"));
    }
    let out = rospm()
        .current_dir(&dir)
        .env("PATH", "")
        .args(["build"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Conda could not be started"));
    assert!(!dir.join(".rospm").exists());
    assert!(!dir.join("ws/build").exists());
}

#[cfg(unix)]
#[test]
fn build_invokes_conda_and_propagates_its_failure_without_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixture("build-conda-execute");
    fs::write(
        dir.join("rospm.toml"),
        format!("{MINIMAL}\n[build.conda]\nname = 'robot'\n"),
    )
    .unwrap();
    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    let conda = bin.join("conda");
    let environment = dir.join("envs/robot");
    fs::create_dir_all(environment.join("conda-meta")).unwrap();
    fs::write(
        environment.join("conda-meta/history"),
        "existing environment",
    )
    .unwrap();
    fs::write(
        &conda,
        "#!/bin/sh\nif [ \"$1\" = env ] && [ \"$2\" = list ]; then\n  printf '{\"envs\":[\"%s\"]}\\n' \"$FAKE_ENV\"\n  exit 0\nfi\n[ \"$5\" = python ] && printf '%s/bin/python\\n' \"$FAKE_ENV\" && exit 0\n[ \"$9\" = 'command -v colcon' ] && echo \"$FAKE_ENV/bin/colcon\" && exit 0\ncase \"$9\" in *rclpy*) echo '{\"python\":\"3.12\",\"ros\":[\"3.12\"],\"failed\":{}}'; exit 0;; esac\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$CONDA_CALL\"\nexit 19\n",
    )
    .unwrap();
    fs::set_permissions(&conda, fs::Permissions::from_mode(0o755)).unwrap();
    let call = dir.join("conda-call");
    let out = rospm()
        .current_dir(&dir)
        .env("PATH", &bin)
        .env("CONDA_CALL", &call)
        .env("FAKE_ENV", &environment)
        .args(["build", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!call.exists());
    let log = dir.join(".rospm/log/build.log");
    assert!(!log.exists());
    // Without a terminal the clean-rebuild fallback never asks or deletes.
    fs::create_dir_all(dir.join("ws/build/pkg")).unwrap();
    let out = rospm()
        .current_dir(&dir)
        .env("PATH", &bin)
        .env("CONDA_CALL", &call)
        .env("FAKE_ENV", &environment)
        .args(["build"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let args = fs::read_to_string(&call).unwrap();
    assert!(
        args.starts_with(&format!(
            "{}\nrun\n--no-capture-output\n-n\nrobot\nbash\n--noprofile\n--norc\n-c\n",
            dir.join("ws").display()
        )),
        "{args}"
    );
    assert!(args.contains("source /opt/ros/jazzy/setup.bash"));
    assert!(args.contains("colcon build"));
    assert!(args.contains("--cmake-args \"-DPython3_EXECUTABLE=$CONDA_PREFIX/bin/python\""));
    assert!(String::from_utf8_lossy(&out.stderr).contains("19"));
    assert!(String::from_utf8_lossy(&out.stdout)
        .contains(environment.join("bin/python").to_str().unwrap()));
    assert!(String::from_utf8_lossy(&out.stdout).contains(log.to_str().unwrap()));
    let saved = fs::read_to_string(&log).unwrap();
    assert!(
        saved.contains("workspace: main\npython: conda -n robot\n"),
        "{saved}"
    );
    assert!(saved.contains("==> exit status: 19"), "{saved}");
    assert!(dir.join("ws/build/pkg").is_dir());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("retry the build"));

    // Unknown bytes must survive the tee; unavailable logs must not hide failure.
    let original_script = fs::read_to_string(&conda).unwrap();
    for unavailable in [false, true] {
        let output = if unavailable {
            "printf '\\033[31mopaque tool failure\\033[0m\\n' >&2\n/bin/rm -f \"$FAILURE_LOG\"\nexit 19\n"
        } else {
            "printf '\\033[31mopaque tool failure\\033[0m\\n' >&2\nexit 19\n"
        };
        fs::write(&conda, original_script.replace("exit 19\n", output)).unwrap();
        for (lang, hint) in [
            ("en", "No supported error format"),
            ("zh", "未识别到支持的错误格式"),
        ] {
            for json in [false, true] {
                let mut command = rospm();
                command
                    .current_dir(&dir)
                    .env("PATH", &bin)
                    .env("CONDA_CALL", &call)
                    .env("FAKE_ENV", &environment)
                    .env("FAILURE_LOG", &log)
                    .args(["--lang", lang, "build"]);
                if json {
                    command.arg("--json");
                }
                let out = command.output().unwrap();
                assert_eq!(out.status.code(), Some(1));
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    stderr.contains("\x1b[31mopaque tool failure\x1b[0m"),
                    "{stderr}"
                );
                if json {
                    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                    assert_eq!(value["ok"], false);
                    assert!(value["error"].as_str().unwrap().contains("19"));
                    assert_eq!(value["build_summary"]["status"], "exit status: 19");
                    assert_eq!(value["build_summary"]["log"], log.to_str().unwrap());
                    assert_eq!(
                        value["build_summary"]["diagnostic_status"],
                        if unavailable {
                            "unavailable"
                        } else {
                            "unrecognized"
                        }
                    );
                } else {
                    let hint = if unavailable {
                        if lang == "en" {
                            "Could not read or locate"
                        } else {
                            "无法读取日志或定位"
                        }
                    } else {
                        hint
                    };
                    assert!(stderr.contains(hint), "{stderr}");
                    assert!(stderr.contains(log.to_str().unwrap()), "{stderr}");
                    assert!(stderr.contains("19"));
                }
                if !unavailable {
                    assert!(fs::read_to_string(&log)
                        .unwrap()
                        .contains("\x1b[31mopaque tool failure\x1b[0m"));
                }
            }
        }
    }
    fs::write(&conda, original_script).unwrap();

    // Exercise real CLI execution/tee, per-package parsing, deduplication and
    // both languages without requiring ROS or a working compiler on the host.
    let script = fs::read_to_string(&conda).unwrap().replace(
        "exit 19\n",
        r#"
/bin/mkdir -p log/latest_build/alpha log/latest_build/beta
/bin/cat > log/latest_build/alpha/stdout_stderr.log <<'LOG'
src/main.cpp:42:10: error: expected ';' before 'return'
src/main.cpp:42:10: error: expected ';' before 'return'
gmake[2]: *** [main.o] Error 1
LOG
/bin/cat > log/latest_build/beta/stdout_stderr.log <<'LOG'
Traceback (most recent call last):
  File "src/node.py", line 7, in <module>
    import numpy
ModuleNotFoundError: No module named 'numpy'
LOG
/bin/cat log/latest_build/alpha/stdout_stderr.log
/bin/cat log/latest_build/beta/stdout_stderr.log
printf 'Failed   <<< alpha [1s, exited with code 2]\nSummary: 0 packages finished\n'
exit 19
"#,
    );
    fs::write(&conda, script).unwrap();
    for (lang, title) in [("en", "Build failure summary"), ("zh", "构建失败摘要")] {
        for json in [false, true] {
            let mut command = rospm();
            command
                .current_dir(&dir)
                .env("PATH", &bin)
                .env("CONDA_CALL", &call)
                .env("FAKE_ENV", &environment)
                .args(["--lang", lang, "build"]);
            if json {
                command.arg("--json");
            }
            let out = command.output().unwrap();
            assert_eq!(out.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&out.stderr);
            if json {
                let value: serde_json::Value = serde_json::from_slice(&out.stdout)
                    .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
                assert_eq!(value["ok"], false);
                assert!(value["error"].as_str().unwrap().contains("19"));
                let summary = &value["build_summary"];
                assert_eq!(summary["log"], log.to_str().unwrap());
                let groups = summary["groups"].as_array().unwrap();
                assert_eq!(groups.len(), 3, "{summary}");
                assert_eq!(groups[0]["package"], "alpha");
                assert_eq!(groups[0]["workspace"], "main");
                assert_eq!(groups[0]["diagnostics"].as_array().unwrap().len(), 1);
                assert_eq!(groups[0]["diagnostics"][0]["location"]["line"], 42);
                assert_eq!(groups[0]["consequences"].as_array().unwrap().len(), 1);
                assert_eq!(groups[1]["package"], "beta");
                assert_eq!(groups[1]["diagnostics"][0]["code"], "module_not_found");
                assert!(groups[1]["diagnostics"][0]
                    .to_string()
                    .contains("conda -n robot"));
                assert!(groups[2]["diagnostics"].as_array().unwrap().is_empty());
                assert_eq!(groups[2]["consequences"].as_array().unwrap().len(), 2);
                assert!(stderr.contains("ModuleNotFoundError"));
            } else {
                assert!(stderr.contains(title), "{stderr}");
                assert!(stderr.contains("[main / alpha]"));
                assert!(stderr.contains("[main / beta]"));
                assert!(stderr.contains("src/main.cpp:42:10"));
                assert!(stderr.contains(log.to_str().unwrap()));
            }
        }
    }
    // A successful run emits no stale diagnostic summary, even with old logs.
    fs::write(
        &conda,
        fs::read_to_string(&conda)
            .unwrap()
            .replace("exit 19", "exit 0"),
    )
    .unwrap();
    let out = rospm()
        .current_dir(&dir)
        .env("PATH", &bin)
        .env("CONDA_CALL", &call)
        .env("FAKE_ENV", &environment)
        .args(["build", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        serde_json::json!({"ok": true})
    );
}

#[test]
fn apply_build_previews_preparation_then_build_and_verify_without_writing() {
    let dir = fixture("apply-build-preview");
    // init picks the host's distro, which apply requires.
    assert!(rospm()
        .current_dir(&dir)
        .arg("init")
        .status()
        .unwrap()
        .success());
    let base = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    let text = format!("{base}\n[build]\nverify = ['ros2 pkg list']\n");
    fs::write(dir.join("rospm.toml"), &text).unwrap();
    let out = rospm()
        .current_dir(&dir)
        .args(["apply", "--build", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = String::from_utf8(out.stdout).unwrap();
    let build = plan.find("colcon build").expect(&plan);
    let verify = plan.rfind("ros2 pkg list").expect(&plan);
    assert!(build < verify, "{plan}");
    // Command line colcon arguments are for `build` only: a restore depends on the manifest alone.
    let out = rospm()
        .current_dir(&dir)
        .args([
            "apply",
            "--build",
            "--dry-run",
            "--",
            "--packages-select",
            "a",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let shown = preview(
        &dir,
        &[
            "main",
            "--",
            "--packages-select",
            "a b",
            "--build-base",
            "b2",
        ],
    );
    assert!(
        shown.contains("--packages-select 'a b' --build-base b2"),
        "{shown}"
    );
    assert!(!preview(&dir, &[]).contains("--build-base"));
    // Plain `build` never runs the verify commands.
    assert!(!preview(&dir, &[]).contains("ros2 pkg list"));
    assert_eq!(fs::read_to_string(dir.join("rospm.toml")).unwrap(), text);
    assert!(!dir.join(".rospm").exists());
}

#[test]
fn no_env_check_skips_the_conda_checks_in_build_and_apply() {
    let dir = fixture("build-no-env-check");
    // init's host-matching distro, so apply plans on any development machine.
    assert!(rospm()
        .current_dir(&dir)
        .arg("init")
        .status()
        .unwrap()
        .success());
    let base = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    fs::write(
        dir.join("rospm.toml"),
        format!("{base}\n[build.conda]\nname = 'robot'\n"),
    )
    .unwrap();
    for args in [
        ["build", "--no-env-check", "--dry-run"],
        ["apply", "--no-env-check", "--dry-run"],
    ] {
        let out = rospm()
            .current_dir(&dir)
            .args(["--lang", "en"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            text.contains("use Conda environment -n robot without checking it (--no-env-check)"),
            "{text}"
        );
        assert!(!text.contains("before building, require"), "{text}");
    }
}
