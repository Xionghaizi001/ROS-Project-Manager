# rospm

Describe, lock and replay ROS 2 project environments from one manifest.

`rospm` wraps rosdep, vcstool, colcon, apt, pixi and docker. It does not replace them.
A project is described by `rospm.toml` (intent) and pinned by `rospm.lock` (result);
with those two files plus the sources, the same environment can be rebuilt on another
machine or another architecture.

Status: **roadmap P1 in progress** (`init --from-env` / `--base` and `check` with host probing done; native `apply` bootstraps the ROS apt source on a bare Ubuntu and `uninstall` plans the removal, both `--dry-run`-able; real-machine `apply`/`build`/`uninstall` pending). **P1.5 (Chinese / English output) done.** See `ROADMAP.md` for the overview, `docs/phases.md` for per-phase checklists and `docs/spec.md` for
the file formats and command semantics. Design decisions live in `docs/decisions.md`.

## Build

```sh
cargo build --release -p rospm-cli      # binary at target/release/rospm
cargo test --workspace
```

## Try it

```sh
rospm init --base desktop               # template; --base ros-base (default) | desktop picks the ROS metapackage
rospm init --from-env                   # draft rospm.toml from this host and directory (TODO-marked)
rospm check                             # validate + expected-vs-actual host report (read-only)
rospm --manifest examples/minimal/rospm.toml check
rospm --manifest examples/minimal/rospm.toml apply --dry-run      # includes the ROS apt source bootstrap when missing
rospm --manifest examples/minimal/rospm.toml uninstall --dry-run  # lists every ros-jazzy-* package it would remove
rospm --manifest examples/minimal/rospm.toml build --dry-run
source <(rospm --manifest examples/minimal/rospm.toml env)
rospm --manifest examples/minimal/rospm.toml apply --dry-run --mirror tuna  # plan against a mirror (tuna | ustc | aliyun)
rospm doctor                            # tools, ROS apt source, and a reachability/speed table of official sources and mirrors
```

Commands that modify the system (`apply`, `uninstall`, `build`, `export`, `import`, `deps install`)
always print their plan first and accept `--dry-run`. `apply` refuses to run on an Ubuntu
release other than the one the manifest names; `uninstall` additionally asks for confirmation
(or `--yes`). Before downloading anything, `apply` probes the official ROS sources and the built-in
mirrors; if the official ones fail it asks (on a terminal) whether to switch to the fastest mirror, or tells
you which `--mirror <name>` (or `ROSPM_MIRROR=<name>`) to pass. The apt signing key is verified against
the built-in fingerprint whichever source it comes from. See `docs/spec.md` §4.1.1.

## Language / 语言切换

Output (help, `check` / `doctor` tables, plan notes, `init` comments, errors) is available in
English and Simplified Chinese. The language is picked before argument parsing, so `--help` follows it:

1. `--lang zh|en` on the command line;
2. `ROSPM_LANG=zh|en`;
3. the locale: `LANGUAGE` (colon-separated list, first `zh*` / `en*` entry wins), then `LC_ALL`,
   `LC_MESSAGES`, `LANG`. A value starting with `zh` (`zh_CN.UTF-8`, `zh_TW`, …) selects Chinese;
   anything else, including unset, `C` and `POSIX`, selects English.

```sh
LANG=zh_CN.UTF-8 rospm check      # 中文输出
rospm --lang en check             # English, whatever the locale
ROSPM_LANG=en rospm check --json  # stable English text in JSON free-text fields
```

Not translated: subcommand and flag names, `rospm.toml` / `rospm.lock` keys and values, `--json`
key names and enum values (`status: "missing"` …), the shell commands in plans, `-v` logs and the
output of the wrapped tools. `rospm init` writes the manifest template of the selected language.

输出语言按 `--lang` > `ROSPM_LANG` > `LANGUAGE` > `LC_ALL` > `LC_MESSAGES` > `LANG` 选择；`zh*` 为中文，其余为英文。
子命令名、参数名、清单键值、`--json` 的键名与枚举值、计划中的 shell 命令不翻译。

## Layout

```
crates/rospm-core   library: manifest, lock, plan, providers, scm
crates/rospm-cli    the `rospm` binary
docs/               spec, decisions, phase checklists, P0 validation report
examples/minimal    smallest valid project (Jazzy / 24.04)
examples/minimal-humble  same project for Humble / 22.04 (CI smoke on that image)
```
