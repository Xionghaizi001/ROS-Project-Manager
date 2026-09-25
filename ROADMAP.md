# ROS Project Manager — 可行性评估与 Roadmap

> 基于 `desc.md`（2026-09-14 版本）编写。工具名 `rospm`（2026-09-15 确认）。
> 配套文档：[决策记录](docs/decisions.md) · [规格草案](docs/spec.md) · [分阶段实施清单](docs/phases.md) · [P0 外部事实核验](docs/p0-external-validation.md)。

## 1. 可行性结论

**结论：可行，但必须严格分期，且个别目标需要改写定义。**

这个项目本质上是"声明式清单 + 锁文件 + 多后端编排层"。每个子能力在生态里都已有成熟的原语可封装，风险不在"能不能做"，而在"范围太宽、每个后端的锁定语义不一致"。

| 目标 | 可行性 | 依托的现有原语 | 备注 |
|---|---|---|---|
| 描述文件 + 重放 | 高 | rosdep / vcstool / colcon / apt / pixi | 核心 MVP |
| 源码锁定 | 高 | `vcs export --exact` | 直接复用 |
| pixi 环境锁定 | 高 | `pixi.lock`（天然多平台） | 委托给 pixi |
| Docker 锁定 | 高 | image digest + buildx | 委托给 docker |
| apt 包版本锁定 | **中** | `snapshots.ros.org`、`snapshot.ubuntu.com` | 见 §2.1 |
| 依赖补全 | 高 | `rosdep install --simulate` + dpkg / pixi list | 差集即可 |
| 跨架构自动替换 | **中低** | 仓库索引查询 + 人工维护映射表 | 需改写定义，见 §2.2 |
| 离线导出 | 中 | `apt-get download`、`pixi-pack`、`docker save` | 跨架构下载可行 |
| 编辑器配置 | 高 | 生成 `.vscode/`、`compile_commands.json`、devcontainer | 低风险，靠后 |
| GUI | 高但成本大 | Tauri / egui | 只做 CLI 的薄壳 |

## 2. 关键风险与需要改写的目标

### 2.1 apt 版本锁定的真实语义

`packages.ros.org` 每个发行版只保留**最新版本**的 .deb，旧版本一旦被 bloom 新版本覆盖就从仓库消失。所以"固定版本 + 从软件源获取"在纯 apt 上不成立。

可行的做法（三选一或组合）：
1. **锁定到快照仓库**：锁文件记录 `snapshots.ros.org/<distro>/<YYYY-MM-DD>` 和 `snapshot.ubuntu.com/<timestamp>`，重放时把 sources.list 指向快照。这是最接近"真锁定"的方案。
2. **锁定 + 本地缓存**：`lock` 时顺手把 .deb 缓存到项目/团队缓存目录，重放优先用缓存（与离线导出共用同一套代码）。
3. **软锁定**：只记录版本用于 `verify` 校验漂移，不承诺能重装。

建议：默认 1，`export` 时自动落成 2，1 不可用时降级为 3 并明确告警。

### 2.2 "跨平台自动替换"需要改写

真正"自动"推断替代包不现实。可实现的版本是：
- **可用性检测自动化**：读取 apt / conda 仓库索引，判断锁文件里每个包在目标架构是否存在。
- **替换规则人工声明**：清单里用 `[target.aarch64]` 段声明覆盖/替换/排除，工具内置一份可更新的常见映射表（如 CUDA、某些驱动、x86-only 的第三方包）。
- 检测到无法解析的包时报告，而不是猜。

### 2.3 native 提供者会修改系统

apt 安装需要 sudo，会污染共享实验室机器和 CI runner。需要在设计中明确：
- native 提供者的"隔离边界"是什么（配置 ROS apt 源 + 装 ROS 底包 + rosdep 系统依赖，其它全部进 workspace）；安装 ROS 本身属于 native `apply` 的范围（决策 #14，spec §4.1），但不安装 / 升级 Ubuntu；`uninstall` 只卸 `ros-<distro>-*` 与 rospm 写的源（决策 #15）；
- 对多人共享机器推荐 pixi 或 docker 提供者；
- `apply` 必须有 `--dry-run` 并输出将执行的命令。

### 2.4 rosdep 与 pixi 的组合

RoboStack 下 rosdep 需要用 RoboStack 自己的 rosdep yaml 源，且解析结果是 conda 包名而非 apt 包名。"依赖管理器"这一层实际上是**按提供者分派**的，抽象要按此设计。

## 3. desc.md 还缺什么

以下是开工前必须补齐的决策，按优先级排列：

1. **清单文件格式与 schema**：建议 TOML（与 pixi、cargo 一致），带 `schema_version` 字段。列出全部字段与默认值。
2. **CLI 动词表**：至少定义 `init / describe / check / apply / build / env / lock / verify / export / import` 的语义，特别是哪些是幂等的、哪些会修改系统。
3. **"描述"的方向**：是从已有环境反推清单（introspect），还是从清单构建环境，或两者都要？建议两者都要，但 MVP 先做后者，introspect 只做 best-effort。
4. **锁文件按提供者分层的语义**：git=commit SHA；apt=版本+快照日期；pixi=引用 `pixi.lock`；docker=digest。哪些字段进 lock、哪些进 manifest，需要白纸黑字。
5. **支持的发行版矩阵**：Humble(22.04) + Jazzy(24.04) 起步，Rolling 只做 CI 冒烟。发行版 EOL 不触发支持降级，Humble 长期保持正式支持。
6. **Python/pip 依赖**：ROS 包大量依赖 pip 包，rosdep 有 pip 键。要么明确纳入依赖管理器，要么明确排除。
7. **多 workspace / underlay 叠加**：ROS 的核心用法之一，清单要能表达"本项目叠在哪个 underlay 之上"。
8. **colcon 配置**：`defaults.yaml`、mixin、`--packages-select` 等编译选项是否进清单。
9. **非目标（Non-goals）清单**：例如 ROS1、Windows/macOS、硬件驱动/内核模块/udev 规则、CUDA 版本管理、Nix。写清楚可以避免范围蔓延。
10. **导出包格式**：目录结构、完整性校验（sha256）、是否签名、体积预期。
11. **测试策略**：容器内集成测试，x86_64 + aarch64（GitHub ARM runner 或 QEMU）矩阵。
12. **工具名与命令名**。
13. **实现语言最终决定**：见 §4。

## 4. 实现语言建议：Rust

- 单一静态二进制，交叉编译到 aarch64 简单，符合"复制到机器人本体就能用"。
- `serde` + `toml`/`serde_yaml`、`clap`、`gix`/`git2`、`reqwest` 覆盖全部需求。
- Pixi 的内核 `rattler` 是 Rust crate，未来若需读取 `pixi.lock`、查询 conda 仓库索引，可直接作库使用而不必解析 CLI 输出。
- apt 一侧建议**先 shell out**（`apt-get`、`apt-cache`、`dpkg-query`），稍后再考虑 `rust-apt` 绑定，避免早期被 libapt-pkg ABI 绑死。
- C++ 的唯一优势是与 ROS 生态同语言，但对本工具（不链接 ROS 库）无实际收益。

## 5. Roadmap 总览

各阶段的逐项任务、完成状态与验收门槛已拆到 [docs/phases.md](docs/phases.md)；本节只保留阶段目标与当前状态。状态更新日期：2026-09-26。

| 阶段 | 目标 | 预估 | 状态 |
|---|---|---|---|
| P0 | 规格、决策、schema、Rust 骨架、CI | 1–2 周 | **基本完成**：决策/spec/schema/骨架/单测 ✅；外部事实 4 项全部有本地实测证据 ✅（ROS 快照签名密钥已核验、补丁版 rosdep 映射 `rclcpp` 通过、x86 上真实 ARM pixi-pack 通过）；CI 各步骤本地复现通过，仅剩 GitHub 实跑与 spec review |
| P1 | native 提供者的描述与重放（x86_64、22.04/24.04、Humble/Jazzy） | 3–4 周 | **进行中**：`init`（含 `--from-env` 反推、`--base` 选底包）与 `check`（环境探测 + 差异报告）已完成；`apply` 从裸 Ubuntu 起步（ROS apt 源 bootstrap）与 `uninstall` 的计划已就绪；`build` / `env` / `shell` / `doctor` 骨架与 `--dry-run` 计划已就绪；缺 fish、干净虚拟机实跑 |
| P2 | 锁文件与依赖补全：`lock` / `apply --locked` / `verify` / `deps` | 3 周 | 未开始（lock schema 与解析已随骨架落地） |
| P3 | Pixi 提供者：生成 `pixi.toml`，锁定委托 `pixi.lock`，RoboStack rosdep 源 | 3 周 | 未开始（占位返回 NotImplemented） |
| P4 | Docker 提供者：Dockerfile / compose / devcontainer，digest 锁定，buildx | 2–3 周 | 未开始（占位返回 NotImplemented） |
| P5 | aarch64 与平台覆盖：仓库索引查询、替换映射表、ARM CI | 3 周 | 未开始（`[target.<arch>]` schema 与合并已实现） |
| P6 | 离线导出与导入：bundle 格式、`export` / `import` / `apply --offline` | 3–4 周 | 未开始 |
| P7 | 编辑器集成与打磨，1.0 发布，冻结 schema v1 | 2 周 | 未开始 |
| P8 | GUI（选装） | 独立排期 | 未开始 |

阶段目标一句话：

- **P0 产出**：`rospm --help` 可运行 ✅；spec 与示例清单待 review。
- **P1 产出**：干净 22.04 虚拟机（无 ROS、无 ROS apt 源）上，仅凭清单 + 源码完成 `apply && build && shell`，`ros2 run` 成功。
- **P2 产出**：两台机器、间隔数周执行 `apply --locked`，`verify` 报告一致。
- **P3 产出**：同一份清单切换 `provider = "pixi"` 后在无 sudo 机器上完成 apply/build。
- **P4 产出**：`rospm apply` 产出可运行镜像，`rospm shell` 进入容器且 ROS 环境就位。
- **P5 产出**：x86 生成的锁文件在 Raspberry Pi / Jetson 上 `apply --locked` 成功，缺失包有明确替代或明确报错。
- **P6 产出**：联网 x86 机器导出，断网 arm64 机器导入并成功编译运行。
- **P7 产出**：1.0 发布。

## 6. 阶段间的硬性依赖

```
P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8
                 └────────┘  (P3/P4 可并行，但都依赖 P2 的 Provider 抽象)
```

P5 依赖 P4 的 buildx 与 P3 的 pixi.lock 多平台能力；P6 依赖 P5 的跨架构下载。

## 7. 建议开工前先读的项目

- RoboStack + Pixi：多平台锁文件与 rosdep 集成方式
- vcstool：`export --exact` 的锁定格式
- snapshots.ros.org / snapshot.ubuntu.com：apt 时间点锁定
- pixi-pack、conda-pack、apt-offline：离线打包思路
- rocker、ade-cli、devcontainers：Docker 封装 ROS 的既有做法
- nix-ros-overlay：完全可复现的对照组（本项目明确不走此路线，但值得参考其难点）
