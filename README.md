# rospm

Describe, lock and replay ROS 2 project environments from one manifest.

`rospm` wraps rosdep, vcstool, colcon, apt, pixi and docker. It does not replace them.
A project is described by `rospm.toml` (intent) and pinned by `rospm.lock` (result);
with those two files plus the sources, the same environment can be rebuilt on another
machine or another architecture.

Status: **roadmap P1 in progress** (`init --from-env` / `--base` and `check` with host probing done; native `apply` bootstraps the ROS apt source on a bare Ubuntu and `uninstall` plans the removal, both `--dry-run`-able; real-machine `apply`/`build`/`uninstall` pending). See `ROADMAP.md` for the overview, `docs/phases.md` for per-phase checklists and `docs/spec.md` for
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
rospm doctor
```

Commands that modify the system (`apply`, `uninstall`, `build`, `export`, `import`, `deps install`)
always print their plan first and accept `--dry-run`. `apply` refuses to run on an Ubuntu
release other than the one the manifest names; `uninstall` additionally asks for confirmation
(or `--yes`).

## Layout

```
crates/rospm-core   library: manifest, lock, plan, providers, scm
crates/rospm-cli    the `rospm` binary
docs/               spec, decisions, phase checklists, P0 validation report
examples/minimal    smallest valid project (Jazzy / 24.04)
examples/minimal-humble  same project for Humble / 22.04 (CI smoke on that image)
```
