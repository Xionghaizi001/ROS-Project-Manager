# ROS Project Manager — 可行性评估与 Roadmap

> 基于 `desc.md`（2026-09-14 版本）编写。工具名 `rospm`（2026-09-15 确认）。

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
- native 提供者的"隔离边界"是什么（只装 ROS 底包 + rosdep 系统依赖，其它全部进 workspace）；
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

## 5. Roadmap

每个阶段末尾都有一个"可演示的产出"，不满足就不进入下一阶段。

### Phase 0 — 规格与骨架（约 1–2 周）
- [x] 补齐 §3 所列决策，写成 `docs/decisions.md` 与 `docs/spec.md`（2026-09-15）。
- [x] 定义 manifest v0 schema 与 lock v0 schema，附示例（`examples/minimal`，spec §2.1）。
- [x] Rust 工程骨架：`clap` CLI、`Provider` trait、`Scm` trait、日志、错误类型、`Plan` + `--dry-run` 基础设施。
- [x] CI：fmt + clippy + 单元测试（x86_64 / arm64）+ Ubuntu 22.04 / 24.04 容器冒烟（`.github/workflows/ci.yml`，尚未在 GitHub 上实际运行）。
- [ ] 验证 spec §8 列出的 4 项外部事实（ROS 快照仓库格式、Ubuntu 快照在 22.04 的用法、pixi-pack aarch64、RoboStack rosdep 源）。
- **产出**：`rospm --help` 可运行 ✅；spec 与示例清单待 review。
- 骨架已超前实现 P1 的部分内容：native 提供者的 `apply` / `build` / `env` / `shell` 计划生成，以及 `init`（无 `--from-env`）、`doctor`。P1 剩余：环境探测（`check` 读取已安装包与 rosdep 状态）、`init --from-env`、在干净虚拟机上实跑。

### Phase 1 — MVP：native 提供者的描述与重放（约 3–4 周）
范围：x86_64、Ubuntu 22.04/24.04、Humble/Jazzy、native(apt) 提供者、git 源码、colcon。
- `rospm init`：生成清单模板；可选从当前目录的 `.repos` / `package.xml` 反推。
- `rospm check`：检查 ROS 底包、rosdep 初始化、所需 apt 包、源码是否存在，输出差异报告，不修改系统。
- `rospm apply`：安装 ROS 底包、`vcs import` 源码、`rosdep install`，全程支持 `--dry-run`。
- `rospm build`：封装 colcon，注入清单中的 colcon 配置。
- `rospm env` / `rospm shell`：生成 source 脚本或启动带环境变量的子 shell（`ROS_DOMAIN_ID`、`RMW_IMPLEMENTATION` 等）。
- **产出**：在一台干净 22.04 虚拟机上，仅凭清单 + 源码完成 `apply && build && shell`，`ros2 run` 成功。

### Phase 2 — 锁文件与依赖补全（约 3 周）
- `rospm lock`：源码 → commit SHA；apt → 已安装版本 + 快照仓库日期；写入 `rospm.lock`。
- `rospm apply --locked`：把 sources.list 切到快照仓库后安装；快照不可用时按 §2.1 降级并告警。
- `rospm verify`：对比锁文件与当前环境，报告漂移。
- `rospm deps check` / `rospm deps install`：解析 workspace 中全部 `package.xml`，与已安装集合求差，补装。
- **产出**：两台机器、间隔数周执行 `apply --locked`，`verify` 报告一致。

### Phase 3 — Pixi 提供者（约 3 周）
- 从清单生成 `pixi.toml`（RoboStack channel、ROS 发行版、任务定义）。
- 锁定委托给 `pixi.lock`；`rospm lock` 只记录其哈希与引用。
- rosdep 切换到 RoboStack 的 yaml 源，`deps` 子命令在此提供者下输出 conda 包名。
- 用第二个实现校正 `Provider` trait，去掉 Phase 1 中的 apt 特有假设。
- **产出**：同一份清单，切换 `provider = "pixi"` 后在无 sudo 的机器上完成 apply/build。

### Phase 4 — Docker 提供者（约 2–3 周）
- 从清单生成 `Dockerfile`（分层：底包 → rosdep 依赖 → 源码 → 编译）、`compose.yaml`、`devcontainer.json`。
- 锁定记录 image digest；`apply` 即 build，`shell` 即 `exec`。
- 用 `buildx` 支持 `--platform linux/arm64`，为 Phase 5 铺路。
- **产出**：`rospm apply` 直接产出可运行的镜像，`rospm shell` 进入容器且 ROS 环境就位。

### Phase 5 — aarch64 与平台覆盖（约 3 周）
- 清单增加 `[target.<arch>]` 段：覆盖、替换、排除。
- `rospm check --target aarch64`：查询 apt/conda 仓库索引，报告目标架构缺失的包。
- 内置可更新的替换映射表，与用户声明合并，用户声明优先。
- CI 增加 aarch64 runner（或 QEMU）矩阵。
- **产出**：x86 上生成的锁文件，在 Raspberry Pi / Jetson 上 `apply --locked` 成功，缺失包有明确替代或明确报错。

### Phase 6 — 离线导出与导入（约 3–4 周）
- 定义 bundle 格式：`manifest + lock + sources/ + payload/ + sha256sums`。
- native：`apt-get download` 求闭包，`dpkg-scanpackages` 生成本地仓库；支持 `dpkg --add-architecture` 跨架构下载。
- pixi：调用 `pixi-pack --platform`。
- docker：`docker save` 指定平台镜像。
- `rospm export --target aarch64` / `rospm import`；导入后走 `apply --locked --offline`。
- **产出**：联网 x86 机器导出，断网 arm64 机器导入并成功编译运行。

### Phase 7 — 编辑器集成与打磨（约 2 周）
- 生成 `.vscode/settings.json`、`c_cpp_properties.json`、`launch.json`；CLion 走 `compile_commands.json`；devcontainer 复用 Phase 4。
- shell 补全、`rospm doctor` 自检、错误信息润色、文档站。
- 1.0 发布，冻结 manifest/lock schema v1。

### Phase 8 — GUI（选装，独立排期）
- Tauri 或 egui，只调用与 CLI 相同的库层，不另写业务逻辑。
- 功能：清单编辑、check/verify 结果可视化、导出向导。

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
