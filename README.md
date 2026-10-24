> [!CAUTION]
> 本项目的 Agent / LLM 含量极高，开发者已尽可能保证可执行文件在运行过程中不会产生损害设备的行为，请在用于生产环境前仔细斟酌，避免影响你的工作。

# rospm

用一份清单描述、锁定并还原 ROS 2 项目环境。

`rospm.toml` 描述项目需求，`rospm.lock` 记录解析结果；配合源码入口和目标平台锁记录，可在另一台满足前置条件的机器上还原、编译和验证项目。rospm 编排 rosdep、vcstool、colcon、apt 和 Conda，依赖求解与构建仍交给这些工具。

## 当前状态

当前版本为 **0.0.1，正在准备 Phase 4 的 1.0 发布**，尚未发布稳定版。清单和锁文件的 `schema_version = 1` 已冻结，字段与校验边界见 [schema v1](docs/schema-v1.md)。

- **native 工作流**：支持初始化、主机检查、依赖安装、源码导入、锁定重放、构建、环境加载与卸载；Ubuntu 22.04 / Humble、24.04 / Jazzy 已有真实验收记录。
- **aarch64**：目标仓库查询、包替换与多平台锁已实现，Docker + QEMU 下完成锁定还原、构建和节点运行；原生 ARM runner 复测仍是发布门槛，不代表 Raspberry Pi / Jetson 驱动已验证。
- **Conda**：支持已有环境及自动安装 Miniforge、创建受管环境；已有环境与 x86_64 干净 Conda 环境一键还原[验收通过](docs/p35-managed-conda-validation.md)。
- **错误诊断**：构建日志、常见 C++ / Python 错误摘要、中英文与 JSON 输出已验收；未知错误保留原始日志和失败状态。
- **后续功能**：完整 TUI、离线导出 / 导入、编辑器集成、GUI、Pixi / Docker 提供者尚未完成，不能作为当前可用工作流。

详细进度见 [阶段清单](docs/phases.md)，发布准备见 [Phase 4 启动清单](docs/p4-release-preparation.md)。

## 从源码构建

需要 Rust stable 与 Cargo。运行 native 安装流程需要与项目清单匹配的 Ubuntu、网络及系统安装权限；ROS 和相关工具可由 `apply` 补齐。

```sh
cargo build --release -p rospm-cli
cargo test --workspace
# 可执行文件：target/release/rospm
export PATH="$PWD/target/release:$PATH"
```

## 快速开始

在项目目录初始化清单，检查并修改生成的 workspace、源码入口与依赖声明：

```sh
rospm init --distro jazzy --base ros-base  # Ubuntu 24.04；22.04 使用 humble
# 已有项目也可使用 rospm init --from-env，审阅生成草稿中的 TODO
rospm check
rospm apply --dry-run                    # 审阅系统与项目变更
rospm apply --build                      # 安装、导入源码、编译并执行 build.verify
rospm check --strict
rospm lock
rospm verify
rospm shell                              # 进入项目环境
```

`--base desktop` 可选择桌面底包。仓库中提供 [Jazzy / 24.04 示例](examples/minimal/rospm.toml) 与 [Humble / 22.04 示例](examples/minimal-humble/rospm.toml)，可用 `--manifest <路径>` 指定清单：

```sh
rospm --manifest examples/minimal/rospm.toml check
rospm --manifest examples/minimal/rospm.toml apply --dry-run
rospm --manifest examples/minimal/rospm.toml build --dry-run
```

将清单、锁文件、源码入口及引用的配置文件纳入版本控制。在另一台匹配的机器上，备齐这些文件后执行：

```sh
rospm apply --locked --build --dry-run
rospm apply --locked --build
rospm verify
```

`--locked` 要求清单哈希一致且锁中有本机平台所需记录；`lock` 只解析本机平台，保留其它平台记录。锁定还原依赖锁中指定的软件源与包仍然可访问，不等于离线还原。较新的非 ROS Ubuntu 系统包可能按规则保留，详见 [锁定与核验语义](docs/spec.md#32-锁定严格度与降级)。

## 常用操作

```sh
rospm doctor                             # 工具、软件源与网络诊断
rospm check --target aarch64              # 查询目标架构仓库覆盖
rospm deps check                         # 检查 rosdep 依赖
rospm deps install --dry-run              # 预览依赖安装
rospm build                              # 编译全部 workspace
rospm build -- --packages-select my_pkg  # 本次透传 colcon 参数
source <(rospm env)                       # 在当前 Bash 中加载环境
rospm apply --dry-run --mirror tuna      # official / tuna / ustc / aliyun
rospm uninstall --dry-run                # 先审阅卸载范围
```

`apply`、`build`、`deps install` 与 `uninstall` 支持 `--dry-run`。`apply` 拒绝与清单不匹配的 Ubuntu 版本；`uninstall` 还要求确认（或显式传 `--yes`），其范围包含清单发行版的 ROS 系统包，应先检查计划。无需默认使用 `sudo rospm`，系统步骤按需提权。

构建与 `apply --build` 的完整输出保存在 `.rospm/log/build.log`，每次执行覆盖。失败摘要用于辅助排查，不替代原始日志；`--json` 提供机器可读结果，构建计划和子进程输出转到 stderr。

## Conda 环境

已有环境可通过清单的 `[build.conda]` 指定 `name` 或 `prefix`，也可仅覆盖本次构建：

```sh
rospm build --conda-env robot-dev
rospm build --conda-prefix /path/to/env
```

已有环境须自行准备兼容 ROS 的 Python、构建工具与依赖，rospm 不接管其包版本。受管模式使用 `[build.conda.managed]` 声明 Python、channels 等，由 `apply` 自动准备；锁定重放还需携带对应平台的 Conda 显式锁文件。Python ABI、配置字段和自动安装规则见 [文件与命令规范](docs/spec.md)。x86_64 干净 Conda 环境端到端验收已通过，复跑脚本与覆盖边界见[验收记录](docs/p35-managed-conda-validation.md)。

## 中文 / 英文输出

```sh
rospm --lang zh --help
rospm --lang en check
ROSPM_LANG=zh rospm build
```

语言优先级为 `--lang` → `ROSPM_LANG` → `LANGUAGE` → `LC_ALL` → `LC_MESSAGES` → `LANG`。`LANGUAGE` 按列表选择首个支持的语言，其余 locale 的 `zh*` 选择中文，默认英文。

命令名、参数名、清单键值、JSON 键名与枚举值、计划中的 shell 命令及外部工具输出不翻译。JSON 自由文本可能随语言变化，需要固定英文时使用 `ROSPM_LANG=en`。当前 `env --shell fish` 尚未完成。

## 项目结构与文档

| 路径 | 内容 |
| --- | --- |
| `crates/rospm-core` | 清单、锁、计划、提供者、源码管理与诊断业务库 |
| `crates/rospm-cli` | `rospm` 命令行入口与中英文输出 |
| `examples/` | 最小项目清单与源码入口 |
| `scripts/` | 锁定重放、QEMU、Conda 与诊断验收脚本 |
| `docs/` | 规范、设计决策、实施清单与实测记录 |

- [路线图](ROADMAP.md) · [阶段清单](docs/phases.md) · [设计决策](docs/decisions.md)
- [文件格式与命令语义](docs/spec.md) · [Phase 4 启动清单](docs/p4-release-preparation.md)
- [native 验收](docs/p1-native-validation.md) · [锁定重放验收](docs/p2-locked-validation.md)
- [aarch64 QEMU 验收](docs/p3-qemu-validation.md) · [已有 Conda 环境验收](docs/p35-existing-conda-validation.md) · [错误诊断验收](docs/p36-diagnostics-validation.md)

## 自行构建与安装（详细步骤）

以下以 Ubuntu 22.04 / 24.04、Bash 为例。在 x86_64 或 aarch64 机器上执行时，默认生成当前机器架构的二进制。编译 rospm 本身不需要预先安装 ROS 或 Conda。

### 1. 准备系统工具与 Rust

```sh
sudo apt-get update
sudo apt-get install -y build-essential curl ca-certificates git
```

尚未安装 Rust 时，通过 rustup 安装 stable 工具链：

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rospm-rustup-init.sh
sh /tmp/rospm-rustup-init.sh -y --profile minimal --default-toolchain stable
source "$HOME/.cargo/env"
```

已有 rustup 时可直接更新工具链；补齐仓库要求的检查组件并确认版本：

```sh
rustup toolchain install stable
rustup component add --toolchain stable rustfmt clippy
rustc --version
cargo --version
```

### 2. 获取源码并编译

下载或克隆本仓库后，在包含 `Cargo.toml` 和 `Cargo.lock` 的仓库根目录执行：

```sh
cd /path/to/ROSProjectManager  # 替换为实际源码目录
cargo build --release --locked -p rospm-cli
./target/release/rospm --version
./target/release/rospm --lang zh --help
```

`--locked` 使用仓库提交的依赖锁文件；首次构建需要联网下载 Rust 依赖。产物为 `target/release/rospm`。

### 3. 执行检查

提交修改或准备分发前，在仓库根目录运行：

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

这些检查不替代真实 ROS 主机、原生 ARM 和 Conda 干净机验收；实测步骤见前面的验收文档。

### 4. 安装到当前用户目录

```sh
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/rospm "$HOME/.local/bin/rospm"
export PATH="$HOME/.local/bin:$PATH"
rospm --version
rospm --lang zh --help
```

若希望新终端也能找到命令，将 `export PATH="$HOME/.local/bin:$PATH"` 加入 `~/.bashrc`（已有该配置时无需重复添加）。更新源码后，重新编译并执行上述 `install` 命令即可更新本地安装。

二进制依赖目标系统的运行时环境。建议在实际使用的 Ubuntu 版本和架构上构建、验证；在较新 Ubuntu 上生成的产物不能直接假定可在较旧版本上运行。
