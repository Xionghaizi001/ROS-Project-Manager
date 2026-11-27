> [!CAUTION]
> 本项目的 Agent / LLM 含量极高，开发者已尽可能保证可执行文件在运行过程中不会产生损害设备的行为，请在用于生产环境前仔细斟酌，避免影响你的工作。

> [!NOTE]
> 项目目前仍在持续更新，但版本发布节奏不会那么快。若你想知道项目目前更新了什么，可以查看 commit 列表；若你想体验最新的功能，可以自行拉取源码并编译。

# rospm

用一份清单描述、锁定并还原 ROS 2 项目环境。

`rospm.toml` 描述项目需求，`rospm.lock` 记录解析结果；配合源码入口和目标平台锁记录，可在另一台满足前置条件的机器上还原、编译和验证项目。rospm 编排 rosdep、vcstool、colcon、apt 和 Conda，依赖求解与构建仍交给这些工具。

## 当前状态

当前版本为 **v1.1.0**。

- **native 工作流**：支持初始化、主机检查、依赖安装、源码导入、锁定重放、构建、环境加载与卸载；在 Ubuntu 22.04 / Humble、24.04 / Jazzy 实机测试通过。
- **aarch64**：arm64 架构已在设计上支持，但未进行实机验证（因为开发者手上暂时没有）。
- **Conda**：支持已有环境及自动安装 Miniforge。新建受管环境默认位于主机用户数据目录 `~/.local/share/rospm/conda/envs/`，支持 `XDG_DATA_HOME`。
- **错误诊断**：构建日志、常见 C++ / Python 错误摘要；未知错误保留原始日志和失败状态。
- **worker CLI**：可显式启动独立任务服务，提交、查询、确认与取消任务；使用方式见 [worker CLI 使用说明](docs/worker-cli.md)。
- **TUI**：键盘 / 鼠标操作、独立环境任务、日志与任务重连。
- **后续功能**：离线导出 / 导入、编辑器集成、GUI、Pixi / Docker 提供者、ROS1 支持；见 [阶段路线图](docs/phases.md)。

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

`--base desktop` 可选择桌面版底包。仓库中提供 [Jazzy / 24.04 示例](examples/minimal/rospm.toml) 与 [Humble / 22.04 示例](examples/minimal-humble/rospm.toml)，可用 `--manifest <路径>` 指定清单：

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

`--locked` 要求清单哈希一致且锁中有本机平台所需记录；`lock` 只解析本机平台，保留其它平台记录。锁定还原依赖锁中指定的软件源与包仍然可访问，不等于离线还原。较新的非 ROS Ubuntu 系统包可能按规则保留。

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

执行时默认仅回显当前步骤，`-v` 或 `-vv` 显示完整执行命令；子进程日志和错误照常输出。`build` 成功后显示“编译成功”。`--dry-run` 和删除操作的确认界面仍显示完整计划。

本机的 `check`、`deps check` 和 TUI 中对应的检查操作会在 rosdep 缓存索引缺失时按项目发行版更新当前用户的缓存。设置 `ROSPM_NO_NETWORK=1` 可跳过更新。

`apply`、`build`、`deps install` 与 `uninstall` 支持 `--dry-run`。`apply` 拒绝与清单不匹配的 Ubuntu 版本；`uninstall` 还要求确认（或显式传 `--yes`），其范围包含清单发行版的 ROS 系统包，应先检查计划。无需默认使用 `sudo rospm`，系统步骤按需提权。

构建与 `apply --build` 的完整输出保存在 `.rospm/log/build.log`，每次执行覆盖。失败摘要用于辅助排查，不替代原始日志；`--json` 提供机器可读结果，构建计划和子进程输出转到 stderr。

## 终端界面与任务恢复

执行 `rospm tui`，用 Tab / 方向键或鼠标切换页面；`l` 查看日志，`p` 打开进程管理。环境任务由当前用户的独立 worker 执行。关闭 TUI 或退出日志跟随不等于取消；重开同一项目可查看原任务，显式继续确认、授权或请求取消。apt / dpkg 等受保护步骤在安全边界响应取消，已完成的更改不会回滚。

系统步骤由前台 sudo 授权，项目 shell、Git、Conda 和构建保持普通用户身份。交互 Shell 的生命周期属于 TUI；它与可断连继续的环境任务不同。SSH 断连存活取决于目标机的 session / cgroup 策略；主机或 worker 故障只恢复状态，不自动重放副作用。

任务日志与结果保存在 `.rospm/worker/tasks/<id>/logs.jsonl` 和 `snapshot.json`，退出后保留，CLI 不自动轮转或清理。用原 ID 查询或继续，先核实 `unknown` 任务的系统状态。操作、保留规则与恢复命令见 [worker 使用说明](docs/worker-cli.md)。

## Conda 环境

已有环境可通过清单的 `[build.conda]` 指定 `name` 或 `prefix`，也可仅覆盖本次构建：

```sh
rospm build --conda-env robot-dev
rospm build --conda-prefix /path/to/env
```

已有环境须自行准备兼容 ROS 的 Python、构建工具与依赖，rospm 不接管其包版本。受管模式使用 `[build.conda.managed]` 声明 Python、channels 等，由 `apply` 自动准备；锁定重放还需携带对应平台的 Conda 显式锁文件。Python ABI、配置字段和自动安装规则见 [文件与命令规范](docs/spec.md)。

## 项目 Python venv

对于 rospm 安装的 Miniforge，可用 `rospm conda set-default` 设为当前用户的默认 Conda，或用 `rospm conda remove-path` 撤销。TUI 的「安装 → Conda」也提供对应操作。设置对新启动的 Bash、Zsh、Fish 会话生效。

`env`、`shell`、`build`、`run` 可用 `--venv` 选择清单同级已有 `.venv`，或用 `--venv-path PATH` 选择已有路径。环境须使用匹配 ROS 的系统 Python 并启用 `--system-site-packages`；不会隐式创建或安装依赖。可用 `rospm venv create` 显式创建项目环境，`--uv` 选择已有 uv 工具，`--dry-run` 预览；已有路径不会覆盖，失败残留不会自动删除。

```sh
rospm build --venv
rospm run --venv package/node
rospm shell --venv
```

显式 venv 覆盖清单 Conda；不传参数则保持原有行为。所选 venv 会清除继承的 Conda/Python 路径并从系统 PATH 组装环境。可用 `rospm venv install --venv -r requirements.txt` 安装显式依赖文件，`--uv` 选择 uv 后端，`--dry-run` 预览。现有 pip 字段只记录，`apply --locked` 不重放 venv，`pyproject.toml` / `uv.lock` 不自动同步。

`rospm check --venv --strict`、`rospm verify --venv` 和 `rospm doctor --venv --no-network` 可诊断所选环境与旧 CMake Python 缓存；`verify` 仍需要锁文件，venv 就绪不代表其依赖已锁定。

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

二进制依赖目标系统的运行时环境。建议在实际使用的 Ubuntu 版本和架构上构建、验证。
