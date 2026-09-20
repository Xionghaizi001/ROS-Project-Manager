# rospm

Describe, lock and replay ROS 2 project environments from one manifest.

`rospm` wraps rosdep, vcstool, colcon, apt, pixi and docker. It does not replace them.
A project is described by `rospm.toml` (intent) and pinned by `rospm.lock` (result);
with those two files plus the sources, the same environment can be rebuilt on another
machine or another architecture.

Status: **roadmap P0 — skeleton**. See `ROADMAP.md` for phases and `docs/spec.md` for
the file formats and command semantics. Design decisions live in `docs/decisions.md`.

## Build

```sh
cargo build --release -p rospm-cli      # binary at target/release/rospm
cargo test --workspace
```

## Try it

```sh
rospm --manifest examples/minimal/rospm.toml check
rospm --manifest examples/minimal/rospm.toml apply --dry-run
rospm --manifest examples/minimal/rospm.toml build --dry-run
source <(rospm --manifest examples/minimal/rospm.toml env)
rospm doctor
```

Commands that modify the system (`apply`, `build`, `export`, `import`, `deps install`)
always print their plan first and accept `--dry-run`.

## Layout

```
crates/rospm-core   library: manifest, lock, plan, providers, scm
crates/rospm-cli    the `rospm` binary
docs/               spec, decisions, roadmap
examples/minimal    smallest valid project
```
