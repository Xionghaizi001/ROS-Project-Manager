use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};
use rospm_core::manifest::ProviderKind;
use rospm_core::mirror;
use rospm_core::probe::{Host, Status};
use rospm_core::provider::{self, Shell};
use rospm_core::run::{
    complete_targets, scan_project, RunRequest, Target, TargetKind, DEBUG_TOOLS,
};

use super::{context, Global, Loaded, VenvArgs};

pub fn run(
    global: &Global,
    venv: super::VenvArgs,
    list: bool,
    interactive: bool,
    requested: Option<&str>,
    args: Vec<String>,
    verbosity: u8,
) -> Result<()> {
    let mut loaded = super::load(global)?;
    let _read_leases = loaded.workspace_read_leases()?;
    venv.select(&mut loaded)?;
    let targets = targets(&loaded)?;
    if list || (requested.is_none() && !interactive) {
        print_targets(&targets);
        return Ok(());
    }
    let target = if interactive {
        choose_interactive(&targets)?
    } else {
        requested
            .and_then(|name| {
                targets
                    .iter()
                    .find(|t| name == t.name || name == format!("{}/{}", t.package, t.name))
            })
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(rospm_core::msg!(
                    "cli.run.not_found",
                    name = requested.unwrap_or_default()
                ))
            })?
    };
    let status = command(&loaded, &target, &args, verbosity, false)?
        .status()
        .context("failed to start bash")?;
    check_status(status)
}

pub(super) fn managed(global: &Global, request: RunRequest) -> Result<()> {
    let mut loaded = super::load(global)?;
    let _read_leases = loaded.read_leases(false)?;
    VenvArgs {
        venv: request.venv,
        venv_path: request.venv_path,
    }
    .select(&mut loaded)?;
    let target = targets(&loaded)?
        .into_iter()
        .find(|target| {
            target.kind == request.kind
                && target.package == request.package
                && target.name == request.name
        })
        .with_context(|| rospm_core::msg!("cli.run.not_found", name = request.name).to_string())?;
    let command = command(&loaded, &target, &request.args, request.verbosity, true)?;
    check_status(super::run_process::supervise(command)?)
}

pub(super) fn debug_tool(manifest: PathBuf, tool: &str) -> Result<()> {
    let loaded = load_debug_project(manifest)?;
    let _read_leases = loaded.read_leases(false)?;
    provider::native::deps::debug_tool_package(loaded.manifest.project.ros_distro, tool)?;
    let missing = rospm_core::msg!("cli.tui.debug.missing", tool = tool).to_string();
    let no_display = crate::i18n::tr("cli.tui.debug.no_display");
    let command = debug_command(&loaded, &["launch", tool, &missing, &no_display])?;
    check_status(super::run_process::supervise(command)?)
}

pub(super) fn missing_debug_package(manifest: PathBuf, tool: &str) -> Result<Option<String>> {
    let loaded = load_debug_project(manifest)?;
    let _read_leases = loaded.read_leases(false)?;
    let package =
        provider::native::deps::debug_tool_package(loaded.manifest.project.ros_distro, tool)?;
    let [status] = probe_debug_tools(&loaded, [tool])?;
    Ok((status == Status::Missing).then_some(package))
}

pub(super) fn debug_tool_statuses(manifest: &Path) -> Result<[Status; DEBUG_TOOLS.len()]> {
    let loaded = load_debug_project(manifest.to_path_buf())?;
    let _read_leases = loaded.read_leases(false)?;
    probe_debug_tools(&loaded, DEBUG_TOOLS)
}

fn probe_debug_tools<const N: usize>(loaded: &Loaded, tools: [&str; N]) -> Result<[Status; N]> {
    let mut args = vec!["probe"];
    args.extend(tools);
    let output = debug_command(loaded, &args)?.output()?;
    anyhow::ensure!(
        output.status.success(),
        "ROS tool probe failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let statuses = output
        .stdout
        .strip_suffix(b"\n")
        .and_then(|bytes| bytes.rsplit(|byte| *byte == b'\n').next());
    let statuses = statuses
        .filter(|bytes| bytes.len() == N && bytes.iter().all(|byte| matches!(byte, b'0' | b'1')))
        .context("invalid ROS tool probe response")?;
    Ok(std::array::from_fn(|index| {
        if statuses[index] == b'1' {
            Status::Ok
        } else {
            Status::Missing
        }
    }))
}

fn load_debug_project(manifest: PathBuf) -> Result<Loaded> {
    let loaded = super::load(&Global {
        manifest: Some(manifest),
        verbose: false,
        json: false,
        interaction: None,
        task: None,
        interactive: false,
    })?;
    anyhow::ensure!(
        loaded.manifest.environment.provider == ProviderKind::Native,
        crate::i18n::tr("cli.tui.debug.no_project")
    );
    Ok(loaded)
}

fn debug_command(loaded: &Loaded, args: &[&str]) -> Result<Command> {
    let host = Host::probe();
    let ctx = context(loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let env = provider::native::Native::ros_env_script(&ctx, Shell::Bash)?;
    let script = format!(
        "set -e\n{env}\n{}",
        r#"
find_debug_executable() {
    executable=
    if [[ $1 == rqt ]]; then
        executable=$(type -P rqt || true)
    else
        local prefix
        local -a prefixes
        IFS=: read -ra prefixes <<< "${AMENT_PREFIX_PATH:-}"
        for prefix in "${prefixes[@]}"; do
            if [[ -n $prefix && -f $prefix/share/ament_index/resource_index/packages/$1 ]]; then
                executable="$prefix/lib/$1/$1"
                break
            fi
        done
    fi
    [[ -f $executable && -x $executable ]]
}
mode=$1
shift
if [[ $mode == probe ]]; then
    printf '\n'
    for tool in "$@"; do
        if find_debug_executable "$tool"; then
            printf 1
        else
            printf 0
        fi
    done
    printf '\n'
    exit 0
fi
if ! find_debug_executable "$1"; then
    printf '%s\n' "$2" >&2
    exit 127
fi
if [[ -z $DISPLAY && -z $WAYLAND_DISPLAY && -z $QT_QPA_PLATFORM ]]; then
    printf '%s\n' "$3" >&2
    exit 1
fi
exec "$executable"
"#
    );
    let mut command = Command::new("/bin/bash");
    command
        .args(["--noprofile", "--norc", "-c", &script, "rospm-debug"])
        .args(args)
        .current_dir(&loaded.root)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin");
    // Keep display and ROS communication variables from the session, while
    // sourcing the requested ROS distro and overlay paths from scratch.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CONDA_")
            || name.starts_with("VIRTUAL_ENV")
            || name.starts_with("PYTHON")
            || matches!(
                name.as_ref(),
                "_CE_CONDA"
                    | "_CE_M"
                    | "BASH_ENV"
                    | "LD_LIBRARY_PATH"
                    | "AMENT_PREFIX_PATH"
                    | "CMAKE_PREFIX_PATH"
                    | "COLCON_PREFIX_PATH"
                    | "ROS_DISTRO"
                    | "ROS_VERSION"
                    | "ROS_PYTHON_VERSION"
                    | "QT_PLUGIN_PATH"
                    | "QT_QPA_PLATFORM_PLUGIN_PATH"
                    | "QML2_IMPORT_PATH"
            )
        {
            command.env_remove(key);
        }
    }
    Ok(command)
}

fn targets(loaded: &Loaded) -> Result<Vec<Target>> {
    let project = scan_project(&loaded.manifest, &loaded.root);
    if !project.built {
        bail!("{}", rospm_core::msg!("cli.run.not_built"));
    }
    if project.targets.is_empty() {
        bail!("{}", rospm_core::msg!("cli.run.none"));
    }
    Ok(project.targets)
}

fn command(
    loaded: &Loaded,
    target: &Target,
    args: &[String],
    verbosity: u8,
    managed: bool,
) -> Result<Command> {
    let host = Host::probe();
    let ctx = context(loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let env = provider::for_manifest(&loaded.manifest).env_script(&ctx, Shell::Bash)?;
    let ros2 = match &loaded.venv {
        Some(path) => format!(
            "{} {}",
            shell_quote(&path.join("bin/python").to_string_lossy()),
            shell_quote(&format!(
                "/opt/ros/{}/bin/ros2",
                loaded.manifest.project.ros_distro
            ))
        ),
        None => "ros2".into(),
    };
    let command = match target.kind {
        TargetKind::Node => format!(
            "{ros2} run {} {}",
            shell_quote(&target.package),
            shell_quote(&target.name)
        ),
        TargetKind::Launch => format!(
            "{ros2} launch{} {} {}",
            if verbosity > 0 { " --debug" } else { "" },
            shell_quote(&target.package),
            shell_quote(&target.name)
        ),
    };
    let script = format!(
        "set -e\n{env}\n{}{command} {}",
        if managed { "exec " } else { "" },
        args.iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "{}",
        rospm_core::msg!(
            "cli.run.running",
            package = &target.package,
            name = &target.name
        )
    );
    if verbosity > 0 {
        println!(
            "$ {}",
            rospm_core::plan::Command::new("bash")
                .args(["--noprofile", "--norc", "-c", &script])
                .cwd(&loaded.root)
                .render()
        );
    }
    io::stdout().flush()?;
    let mut command = Command::new("bash");
    command
        .args(["--noprofile", "--norc", "-c", &script])
        .current_dir(&loaded.root);
    Ok(command)
}

fn check_status(status: std::process::ExitStatus) -> Result<()> {
    if !status.success() {
        bail!("{}", rospm_core::msg!("cli.run.failed", status = status));
    }
    Ok(())
}

fn print_targets(targets: &[Target]) {
    println!("{}", rospm_core::msg!("cli.run.select"));
    let ordered = display_order(targets);
    for (kind, heading) in [
        (TargetKind::Node, rospm_core::msg!("cli.run.nodes")),
        (TargetKind::Launch, rospm_core::msg!("cli.run.launch_files")),
    ] {
        println!("\n{heading}");
        let mut group = ordered
            .iter()
            .enumerate()
            .filter(|(_, target)| target.kind == kind)
            .peekable();
        if group.peek().is_none() {
            println!("  {}", rospm_core::msg!("cli.run.empty_group"));
        }
        for (index, target) in group {
            println!("  {}. {}/{}", index + 1, target.package, target.name);
        }
    }
}

// Numbering and selection must use the same order across both categories.
fn display_order(targets: &[Target]) -> Vec<&Target> {
    [TargetKind::Node, TargetKind::Launch]
        .into_iter()
        .flat_map(|kind| targets.iter().filter(move |target| target.kind == kind))
        .collect()
}

fn resolve_selection<'a>(targets: &'a [Target], input: &str) -> Option<&'a Target> {
    let input = input.trim();
    if input.bytes().all(|byte| byte.is_ascii_digit()) {
        return input
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| display_order(targets).get(index).copied());
    }
    targets.iter().find(|target| {
        input == target.name || input == format!("{}/{}", target.package, target.name)
    })
}

fn choose_interactive(targets: &[Target]) -> Result<Target> {
    if !io::stdin().is_terminal() {
        bail!("{}", rospm_core::msg!("cli.run.select_required"));
    }
    print_targets(targets);
    let prompt = rospm_core::msg!("cli.run.prompt");
    print!("{prompt} ");
    io::stdout().flush()?;
    let term = TerminalMode::new()?;
    let mut input = String::new();
    let mut stdin = io::stdin();
    let mut byte = [0u8; 1];
    loop {
        stdin.read_exact(&mut byte)?;
        match byte[0] {
            // cfmakeraw disables ISIG: Ctrl+C arrives as a byte, not SIGINT.
            // Restore the terminal before reporting cancellation; never exit
            // directly here, since that would skip the terminal guard's Drop.
            3 => {
                drop(term);
                println!("^C");
                return Err(rospm_core::Error::Cancelled.into());
            }
            b'\n' | b'\r' => {
                println!();
                break;
            }
            8 | 127 => {
                if input.pop().is_some() {
                    print!("\x08 \x08");
                    io::stdout().flush()?;
                }
            }
            b'\t' => {
                let matches = complete_targets(targets, &input);
                if matches.len() == 1 {
                    input = matches[0].clone();
                    print!("\r\x1b[2K{prompt} {input}");
                    io::stdout().flush()?;
                } else if !matches.is_empty() {
                    println!();
                    for m in &matches {
                        println!("  {m}");
                    }
                    print!("{prompt} {input}");
                    io::stdout().flush()?;
                }
            }
            c if c.is_ascii_graphic() || c == b' ' => {
                input.push(c as char);
                print!("{}", c as char);
                io::stdout().flush()?;
            }
            _ => {}
        }
    }
    drop(term);
    resolve_selection(targets, &input)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!(rospm_core::msg!("cli.run.not_found", name = input)))
}

struct TerminalMode {
    original: libc::termios,
}
impl TerminalMode {
    fn new() -> Result<Self> {
        let fd = 0;
        let mut original = std::mem::MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(anyhow::anyhow!("cannot read terminal settings"));
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        // Only input needs raw mode; keep newline-to-CRLF output processing.
        raw.c_oflag = original.c_oflag;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self { original })
    }
}
impl Drop for TerminalMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.original);
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_categories_even_when_source_order_is_interleaved() {
        let targets: Vec<_> = [
            (TargetKind::Launch, "a", "start.py"),
            (TargetKind::Node, "b", "talker"),
            (TargetKind::Launch, "b", "start.py"),
            (TargetKind::Node, "c", "listener"),
        ]
        .into_iter()
        .map(|(kind, package, name)| Target {
            kind,
            package: package.into(),
            name: name.into(),
            path: Default::default(),
        })
        .collect();
        for (input, original_index) in [("1", 1), ("2", 3), ("3", 0), ("4", 2)] {
            assert_eq!(
                resolve_selection(&targets, input),
                Some(&targets[original_index])
            );
        }
        for input in ["0", "5", "999999999999999999999999999", "", "missing"] {
            assert!(resolve_selection(&targets, input).is_none());
        }
        assert_eq!(
            resolve_selection(&targets, " b/start.py "),
            Some(&targets[2])
        );
        assert_eq!(resolve_selection(&targets, "talker"), Some(&targets[1]));
    }
}
