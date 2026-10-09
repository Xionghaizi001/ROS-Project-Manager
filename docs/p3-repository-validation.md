# P3 目标仓库索引查询验证

日期：2026-10-09。主机：Ubuntu 22.04 x86_64；工具链：Rust / Cargo 1.99.0，安装在项目 `.tools` 内。执行 `source .tools/env.sh` 可在当前 Bash 会话使用工具链；依赖下载镜像配置仅位于 `.tools/cargo/config.toml`，不修改用户全局配置。

## 实现范围

- 显式 `check --target` 才查询索引。支持 `aarch64` / `arm64` / `linux-aarch64`，以及 x86_64 对应别名；清单中的平台名称继续使用规范名称。
- native 查询 ROS 官方 apt 索引及 Ubuntu release / updates / security × 四个组件，ARM 使用 ports，检查底包、三个工具包与平台覆盖/替换/排除后的 apt 包。
- pixi 读取配置的清单，缺失时仅在内存生成默认配置；查询每个 channel 的目标子目录与 `noarch`，支持 `.tar.bz2` 与 `.conda` 两类索引条目。包含普通、host、build 及适用 target 依赖；feature 环境报告未知，不声称已检查。
- 报告只表示包名可用性，不求解版本和传递依赖，不表示目标主机已就绪。JSON 新增 `repositories.items`。缺包为 `missing`，下载/解压/解析失败为 `unknown`；两者都会使 `--strict` 退出 1，普通检查仍退出 0。

## 自动化验证

```bash
source .tools/env.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p rospm-cli
```

上述检查通过。测试合计 158 项通过、0 失败、5 项既有联网测试按原 `#[ignore]` 配置忽略。全量测试日志保存在本机 `.tools/test-workspace.log`（不提交）。

新增覆盖：apt 架构目录及全部组件、`Architecture: all`、过滤错误架构、空索引、损坏索引、平台替换与排除、部分仓库失败时不误报缺包；conda 两种包格式、目标依赖、配置格式错误、默认清单与 `noarch`；CLI 断网与别名、严格模式退出码，以及本地 HTTP 服务上的真实压缩索引下载、存在/缺失包、中英文输出与文件不变性。

全量回归发现旧 `apply --locked --dry-run` 测试默认假设 ROS 源可由 rospm 管理。本机源为 `/etc/apt/sources.list.d/ros-fish.list`，产品按设计保留外来文件；测试现在依据实际源路径分别断言“保留并提示”或“配置快照”。独立的合成主机计划测试继续覆盖这两种分支，未修改主机源或 keyring。

## 真实网络查询

native：

```bash
target/debug/rospm --manifest examples/minimal/rospm.toml --lang en \
  check --target aarch64 --strict --json
```

实际解析目标索引后找到 `ros-jazzy-ros-base`、`python3-rosdep`、`python3-vcstool`、`python3-colcon-common-extensions` 共 4 个包。Ubuntu ports 有 4 个索引请求因 TLS 连接提前结束失败，报告为 `unknown`，严格模式退出 1。JSON 证据保存在本机 `.tools/check-aarch64.json`。

conda：使用 `.tools/pixi-check/rospm.toml`（Jazzy、pixi、目标 `linux-aarch64`，没有现存 `pixi.toml`），执行同样的严格检查。真实 RoboStack / conda-forge 索引中找到 8 个包；`conda-forge/noarch` 请求因 TLS 连接提前结束失败，`vcstool` 因索引不完整报告 `unknown`，严格模式退出 1。JSON 证据保存在本机 `.tools/check-conda-aarch64.json`。

这两次网络检查验证了真实索引解析及部分失败行为，不能作为所有官方索引均可达的证据。全部索引成功、缺包与失败时的判定由隔离测试覆盖；本项不包含后续 QEMU / ARM runner 安装、构建与运行验收。
