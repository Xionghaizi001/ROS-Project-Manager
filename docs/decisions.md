# 设计决策记录（P0）

> 2026-09-15 确认。编号对应 `ROADMAP.md` §3。工具名 `rospm`（原占位 `rpm` 与 RedHat 包管理器冲突，已弃用）。

| # | 议题 | 决策 | 备注 / 派生约束 |
|---|---|---|---|
| 1 | 清单格式 | TOML，结构参照 `pixi.toml` / `Cargo.toml`；文件名 `rospm.toml`；顶层 `schema_version` | schema 见 `spec.md` §2 |
| 2 | 命令语义分级 | **只读幂等**：`check` `verify` `deps check` `env` `doctor`。**写项目文件、幂等**：`init` `lock` `editor`。**修改系统或产出产物**：`apply` `build` `export` `import` `deps install` | 第三类必须支持 `--dry-run`，并在执行前打印计划 |
| 3 | 描述方向 | 双向。MVP 只做"清单 → 环境"；"环境 → 清单"（`init --from-env`）为 best-effort，只填能可靠探测的字段 | 探测不到的字段留空并在输出中标注 `# TODO` |
| 4 | 锁文件分层 | **自带锁机制的提供者（pixi）**：清单只引用其清单文件（`pixi.toml`），其锁文件（`pixi.lock`）与项目一同提交，`rospm.lock` 仅记录其 sha256 用于校验。**松散管理的对象（apt / git / docker 镜像）**：由 `rospm.lock` 直接锁定 | 不把 pixi 的解析结果复制进 `rospm.toml`，避免双份真相 |
| 5 | 发行版矩阵 | Humble(22.04) + Jazzy(24.04) 正式支持；Rolling 仅 CI 冒烟 | **发行版 EOL 不触发支持降级**：EOL 后仍有大量在用部署，Humble 在可预见的相当长时间内保持正式支持；快照仓库对 EOL 发行版反而更重要（决策 2026-09-15 补充） |
| 6 | pip 依赖 | 清单中**只作记录**（`[dependencies].pip`），不由本工具安装；实际安装交给 rosdep 的 pip 键 | `lock` 阶段仍记录 `pip freeze` 结果供 `verify` 对比，但不承诺重装 |
| 7 | 多 workspace / 叠加顺序 | 清单显式声明各 workspace 及其 `underlays`；工具据此做拓扑排序得出**编译 / 安装顺序**；单个 workspace 内部的包顺序仍交给 colcon | 清单中出现环依赖 → 校验失败 |
| 8 | colcon 配置 | 纳入清单管理（`[workspaces.<name>.colcon]`）；新建环境时必须由清单驱动；从已有环境反推时不承诺还原 | 与 #7 直接相关：叠加顺序 + colcon 参数共同决定可复现的编译结果 |
| 9 | 非目标 | 只作记录，见 `spec.md` §7 | 用于拒绝范围蔓延 |
| 10 | 导出包格式 | `manifest + data` 结构；**校验必选**（SHA256SUMS）；**签名可选**；**压缩可选** | 格式见 `spec.md` §5 |
| 11 | 测试策略 | 容器内集成测试，x86_64 + aarch64 矩阵 | 见 `spec.md` §6 |
| 12 | 工具名 / 命令名 | **`rospm`**（2026-09-15 确认） | 清单 `rospm.toml`，锁文件 `rospm.lock`，缓存目录 `.rospm/` |
| 13 | 实现语言 | Rust | apt 侧先 shell out，不绑定 libapt-pkg |
| 14 | 安装 ROS（2026-09-26 补充） | **属于 native 提供者 `apply` 的范围**：配置 ROS apt 源（locale、universe、keyring、`sources.list.d/ros2.list`）→ 装底包。rospm 自己写 keyring + 源文件，不安装 `ros2-apt-source` 包 | 理由：pixi / docker 提供者天然包含安装 ROS，native 不补上则"同一份清单切换提供者"残缺，且 P1 验收（干净虚拟机）无法通过；源文件由 rospm 管理是为了 P2 快照源切换复用同一路径。不安装 / 升级 Ubuntu 本身，版本不符直接报错。见 `spec.md` §4.1 |
| 15 | 卸载 ROS（2026-09-26 补充） | `rospm uninstall`：显式列出已装 `ros-<distro>-*` 后 `apt-get remove`（不 `autoremove`，只提示，2026-10-20 修订）；rospm 写的 apt 源随之删除，但有其它发行版或源非 rospm 所写时保留；不卸 rosdep / vcstool / colcon；不动项目文件 | 第三类命令，`--dry-run` 必须；额外要求交互确认或 `--yes`。底包口味由 `init --base ros-base\|desktop` 选择，清单字段 `ros_base` 仍为自由包名 |
| 16 | 输出语言 / i18n（2026-09-26 补充） | **P1.5 单独一小段排期**，在 P2 之前落地：初期只做简体中文 + 英文；按系统 locale 自动选择（`LANGUAGE` → `LC_ALL` → `LC_MESSAGES` → `LANG`，`zh*` → 中文，其余 → 英文），`ROSPM_LANG=zh\|en` / `--lang` 可覆盖。翻译资源编译进二进制（单文件分发不变） | 理由：用户日常读中文；越晚做，回填字符串越贵。P1.5 之后新增的所有面向用户的字符串必须走 i18n 层。机器接口不翻译：`--json` 的键名与枚举值、清单 / 锁文件 / 计划里的 shell 命令、日志。见 `spec.md` §4.2 与 `phases.md` Phase 1.5 |
| 17 | 源镜像与网络探测（2026-09-26 补充） | native `apply` 在第一次下载前先并行测速：官方 ROS apt（`dists/<codename>/InRelease`）、rosdistro（`rosdep/base.yaml`，约 270 KB，测吞吐并捕捉中途停滞）、`.repos` 里的 git 主机。官方任一失败而某镜像全部可达时，**交互终端下询问**是否切到最快的镜像，非终端则报错并给出 `--mirror <name>`；`--mirror` / `ROSPM_MIRROR` 显式指定则不问。内置镜像表：`tuna`、`ustc`（apt + rosdistro）、`aliyun`（仅 apt）。换源影响 `ros2.list` 的 URL、`ros.key` 来源、rosdep 的 `20-default.list` 与 `ROSDISTRO_INDEX_URL`；不改 Ubuntu 自身的源，git 主机无镜像只告警 | 理由：P1 实机验证中 GitHub 连接反复停滞（密钥下载挂起、`rosdep update` 超时），国内环境这是常态。**绝不静默换源**（apt 源是系统配置）；密钥无论来自哪里都按内置指纹校验，镜像不可能替换密钥；`--dry-run` 不测速以保持离线可用与确定性；`doctor` 展示同一张测速表。见 `spec.md` §4.1.1 |
| 18 | 指定 ROS 发行版（2026-10-19 补充） | `apply --distro <名>` 与清单不同时，**写回清单的 `ros_distro`**（`ros_base` 若为 `ros-<旧发行版>-*` 一并改名），再按新发行版执行；发行版随项目走，不做一次性覆盖。主机 Ubuntu 不符则报错且不改清单；`--locked` 下发行版不同直接报错 | 理由：单一真相——`lock` / `verify` / `build` / `env` / `uninstall` 都读清单，一次性覆盖会让后续命令与已装系统不一致。见 `phases.md` |

## 派生原则（由上述决策推出，后续实现须遵守）

1. **单一真相**：一个事实只存在于一个文件里。清单存意图，锁文件存结果，提供者自带的锁不复制。
2. **计划先于执行**：所有第三类命令先生成计划（将执行的 shell 命令、将写入的文件），`--dry-run` 只打印计划。
3. **顺序显式化**：任何跨 workspace 的编译或安装顺序都必须能从清单推导，不依赖用户手工先后执行。
4. **降级必告警**：锁定、快照、跨架构替换等任何一步做不到"严格"时，工具降级执行但必须明确输出降级原因。
