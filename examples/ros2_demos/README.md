# ros2_demos — manual test project

Sources: [ros2/demos](https://github.com/ros2/demos) and
[ros2/examples](https://github.com/ros2/examples), branch `jazzy`
(43 `package.xml` files). `ws/` is git-ignored; fetch it with either

```sh
cd examples/ros2_demos
vcs import --input main.repos ws/src          # what `rospm apply` runs
# or, without vcstool:
git clone --depth 1 -b jazzy https://github.com/ros2/demos.git    ws/src/demos
git clone --depth 1 -b jazzy https://github.com/ros2/examples.git ws/src/examples
```

Things to try (all read-only except `apply`/`build` without `--dry-run`):

```sh
cd examples/ros2_demos
rospm check                        # expected-vs-actual table for this host
rospm check --strict; echo $?      # 1 unless the host is fully ready
rospm check --target linux-aarch64 # skips host probing, applies overrides only
rospm check --json | jq .environment

mv rospm.toml rospm.toml.bak
rospm init --from-env              # draft from ws/src + main.repos; compare with the .bak
mv rospm.toml.bak rospm.toml

rospm apply --dry-run
rospm build --dry-run
rospm doctor
```

To test against Humble on Ubuntu 22.04, change `ros_distro`, `ubuntu`,
`ros_base` and the two `version:` lines in `main.repos` to `humble`.
