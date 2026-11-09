use std::io::{self, IsTerminal, Read, Write};
use std::process::Command;

use anyhow::{bail, Context as _, Result};
use rospm_core::mirror;
use rospm_core::probe::Host;
use rospm_core::provider::{self, Shell};
use rospm_core::run::{complete_targets, scan_workspace, Target, TargetKind};

use super::{context, load, Global};

pub fn run(
    global: &Global,
    venv: super::VenvArgs,
    list: bool,
    interactive: bool,
    requested: Option<&str>,
    args: Vec<String>,
) -> Result<()> {
    let mut loaded = load(global)?;
    venv.select(&mut loaded)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let mut workspaces = Vec::new();
    for ws in loaded.manifest.workspaces.values() {
        workspaces.push(scan_workspace(&loaded.root.join(&ws.path)));
    }
    if workspaces.iter().all(|w| !w.built) {
        bail!("{}", rospm_core::msg!("cli.run.not_built"));
    }
    let targets: Vec<Target> = workspaces.into_iter().flat_map(|w| w.targets).collect();
    if targets.is_empty() {
        bail!("{}", rospm_core::msg!("cli.run.none"));
    }
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
            "{ros2} launch {} {}",
            shell_quote(&target.package),
            shell_quote(&target.name)
        ),
    };
    let script = format!(
        "set -e\n{env}\n{command} {}",
        args.iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let status = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", &script])
        .current_dir(&loaded.root)
        .status()
        .context("failed to start bash")?;
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
