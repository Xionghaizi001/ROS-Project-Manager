//! Real HTTP transport and CLI exit codes, using a local conda repository.
mod common;

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn target_conda_repository_reports_available_missing_and_localized_output() {
    let target = if cfg!(target_arch = "aarch64") {
        "linux-x86_64"
    } else {
        "linux-aarch64"
    };
    let subdir = if cfg!(target_arch = "aarch64") {
        "linux-64"
    } else {
        "linux-aarch64"
    };
    for (package, lang, json, success) in [
        ("available", "en", true, true),
        ("missing", "en", true, false),
        ("available", "zh", false, true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let mut zip = Command::new("bzip2")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        zip.stdin
            .take()
            .unwrap()
            .write_all(
                br#"{"packages":{},"packages.conda":{"available.conda":{"name":"available"}}}"#,
            )
            .unwrap();
        let output = zip.wait_with_output().unwrap();
        assert!(output.status.success());
        let body = output.stdout;
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut paths = vec![];
            while paths.len() < 2 {
                assert!(
                    Instant::now() < deadline,
                    "repository requests did not arrive"
                );
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                }
                let request = String::from_utf8(request).unwrap();
                paths.push(request.split_whitespace().nth(1).unwrap().to_owned());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            paths
        });
        let dir = common::fixture(&format!("repo-{package}-{lang}"));
        fs::write(
            dir.join("rospm.toml"),
            format!(
                r#"
schema_version = 1
[project]
name = "repository-check"
ros_distro = "jazzy"
platforms = ["{target}"]
[environment]
provider = "pixi"
[environment.pixi]
[workspaces.main]
path = "ws"
"#
            ),
        )
        .unwrap();
        let pixi = format!(
            "[workspace]\nchannels = [\"http://{address}\"]\n[dependencies]\n{package} = \"*\"\n"
        );
        fs::write(dir.join("pixi.toml"), &pixi).unwrap();
        let mut cmd = common::rospm();
        cmd.current_dir(&dir)
            .env_remove("ROSPM_NO_NETWORK")
            .args(["--lang", lang, "check", "--target", target, "--strict"]);
        if json {
            cmd.arg("--json");
        }
        let out = cmd.output().unwrap();
        let paths = server.join().unwrap();
        assert_eq!(
            paths,
            vec![
                format!("/{subdir}/repodata.json.bz2"),
                "/noarch/repodata.json.bz2".into()
            ]
        );
        assert_eq!(
            out.status.success(),
            success,
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if json {
            let report = common::json(&out);
            assert_eq!(report["environment"]["probed"], false);
            assert_eq!(
                report["repositories"]["items"][0]["status"],
                if success { "ok" } else { "missing" }
            );
        } else {
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(text.contains("仓库包可用性"));
            assert!(!text.contains("本机与清单一致"));
        }
        assert_eq!(fs::read_to_string(dir.join("pixi.toml")).unwrap(), pixi);
        assert!(!dir.join(".rospm").exists());
    }
}
