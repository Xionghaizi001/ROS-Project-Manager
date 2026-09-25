# rospm 分阶段实施清单

> 从 `ROADMAP.md` §5 拆出，记录每个阶段的具体任务与完成状态。状态更新日期：2026-09-25。
> 总览、可行性与风险分析仍在 [ROADMAP.md](../ROADMAP.md)；决策见 [decisions.md](decisions.md)；格式与命令语义见 [spec.md](spec.md)。

图例：`[x]` 完成 · `[~]` 部分完成（子项说明） · `[ ]` 未开始。
每个阶段末尾的"产出"是进入下一阶段的门槛，不满足不进入下一阶段。

---

## Phase 0 — 规格与骨架（约 1–2 周）

状态：**基本完成**，仅外部事实核验与 review 未收口。

- [x] 补齐 ROADMAP §3 所列 13 项决策，写成 `docs/decisions.md` 与 `docs/spec.md`（2026-09-15）。
- [x] 定义 manifest v0 schema 与 lock v0 schema，附示例（`examples/minimal`，spec §2.1 / §3.1）。
- [x] Rust 工程骨架：`clap` CLI、`Provider` trait、`Scm` trait（`Vcstool`）、日志、错误类型、`Plan` + `--dry-run` 基础设施。
  - [x] `rospm-core`：`manifest` / `lock` / `plan` / `platform` / `provider` / `scm` 模块。
  - [x] 单元测试：清单解析与校验、拓扑排序、平台覆盖合并、TOML 往返、native 计划生成（`cargo test --workspace` 全绿）。
- [~] CI：fmt + clippy + 单元测试（x86_64 / arm64）+ Ubuntu 22.04 / 24.04 容器冒烟。
  - [x] `.github/workflows/ci.yml` 已编写。
  - [ ] 在 GitHub 上实际跑通一次（含 `ubuntu-24.04-arm` runner）。
- [~] 验证 spec §8 列出的 4 项外部事实。实测过程与证据见 [p0-external-validation.md](p0-external-validation.md)。
  - [x] Ubuntu 22.04 快照：APT 2.4.11+ 已回移植；`--snapshot` 与直接 URL 两种方式在 jammy/main/amd64 上实测通过。
  - [x] ROS 快照：目录布局 `snapshots.ros.org/<distro>/<date>/ubuntu/…`，日期不定期；索引大小与 SHA256 与 Release 一致；Rolling 当前 404。
  - [ ] ROS 快照 `Release.gpg` 签名密钥来源核验（当前 `ros.key` 无法验证签名，禁止 `trusted=yes` 绕过）。
  - [x] RoboStack rosdep 源链、`--os=conda:linux` 系统键映射、`skip_keys` 只作用于 install/check 的语义。
  - [ ] conda-forge 补丁版 rosdep（0.26.0 build 1）对 ROS 包（`rclcpp`）的 conda 映射对照。
  - [ ] pixi 0.81.0 完整下载并校验，`pixi lock` + `pixi-pack --platform linux-aarch64` 真实打包（此前被下载中断 / 审批服务 503 阻断）。
- [ ] spec 与示例清单 review。
- **产出**：`rospm --help` 可运行 ✅。

> 骨架已超前实现 P1 的部分内容（见下），因此 P1 从"补探测与实跑"开始，而不是从零开始。

---

## Phase 1 — MVP：native 提供者的描述与重放（约 3–4 周）

范围：x86_64、Ubuntu 22.04/24.04、Humble/Jazzy、native(apt) 提供者、git 源码、colcon。
状态：**进行中**。命令骨架与计划生成已就绪；缺环境探测、反推、真实机器验证。

### `rospm init`
- [x] 生成清单模板（`templates/rospm.toml`），已存在需 `--force`，模板自校验。
- [ ] `--from-env`：best-effort 探测（决策 #3），探测不到的字段留空并标 `# TODO`。
- [ ] 从当前目录的 `.repos` / `package.xml` 反推 workspaces 与依赖。

### `rospm check`
- [x] 清单校验、拓扑排序、平台覆盖合并、`--target <platform>`、`--json` 输出。
- [ ] 环境探测：ROS 底包是否安装、rosdep 是否已 `init`/`update`、所需 apt 包已安装集合、源码是否已 import。
- [ ] 输出差异报告（期望 vs 实际），不修改系统。

### `rospm apply`
- [x] native 计划生成：底包 → `vcs import` → `rosdep install` → 额外 apt；`--dry-run` 打印计划。
- [x] `--locked` 在 P2 前明确降级告警；`--offline` 返回 NotImplemented(P6)。
- [ ] 非 dry-run 在真实机器上执行并处理失败（sudo 交互、apt 锁、rosdep 未初始化等）。

### `rospm build`
- [x] 封装 colcon，按叠加顺序 source underlay，注入清单中的 colcon 配置；`--dry-run`。
- [ ] 真实机器上验证 `[workspaces.<name>.colcon]` 各字段的实际效果。

### `rospm env` / `rospm shell`
- [x] `env --shell bash|zsh`；按 workspace 顺序 source，导出 `[env]`。
- [ ] `env --shell fish`（spec §4 已列出）。
- [x] `shell`：写入 `.rospm/env.sh` 与 `shellrc`，启动带前缀提示符的交互 bash。

### `rospm doctor`
- [x] 检测 git / vcs / colcon / rosdep / apt-get / dpkg-query / pixi / docker 与 `/opt/ros/*`，`--json`。

### 验收
- [ ] 在一台干净 22.04 虚拟机上，仅凭清单 + 源码完成 `apply && build && shell`，`ros2 run` 成功。
- [ ] 同样流程在 24.04 + Jazzy 上通过。

---

## Phase 2 — 锁文件与依赖补全（约 3 周）

状态：**未开始**。`rospm.lock` 的数据结构与读取（`lock.rs`）已随骨架落地；`lock` / `verify` / `deps` 命令目前返回 NotImplemented(P2)。
前置：P0 的"ROS 快照签名密钥"核验必须先完成，否则 `apply --locked` 无法可信地切换源。

### `rospm lock`
- [x] lock v0 schema 与解析（`Lock::parse`）。
- [ ] 源码层：`vcs export --exact` → commit SHA。
- [ ] apt 层：按平台记录已安装版本 + ROS / Ubuntu 快照日期。快照日期必须**发现**真实存在的日期并验证 suite、架构、签名，不能把请求日期直接拼进 URL。
- [ ] pip 层：`pip freeze` 结果只记录供 `verify` 对比（决策 #6）。
- [ ] 记录清单哈希；同一输入产生同一输出（幂等）。
- [ ] 锁文件读写往返测试。

### `rospm apply --locked`
- [ ] 把 sources.list 切到 `snapshots.ros.org` 与 `snapshot.ubuntu.com`（22.04 用 `--snapshot` 或直接 URL，保留 keyring 签名验证）。
- [ ] 覆盖 updates / security 组件与 arm64（P0 只冒烟了 jammy/main/amd64）。
- [ ] 快照不可用时按 spec §3.2 / ROADMAP §2.1 降级并明确告警。

### `rospm verify`
- [ ] 对比锁文件与当前环境：源码 SHA、apt 版本、pip 版本；清单哈希不一致提示重新 `lock`。

### `rospm deps check` / `rospm deps install`
- [ ] 解析 workspace 中全部 `package.xml`，与已安装集合求差。
- [ ] `install` 交给 rosdep 执行，支持 `--dry-run`。

### 验收
- [ ] 两台机器、间隔数周执行 `apply --locked`，`verify` 报告一致。
- [ ] CI 增加 `lock` → 新容器 `apply --locked` → `verify` 场景。

---

## Phase 3 — Pixi 提供者（约 3 周）

状态：**未开始**。`ProviderKind::Pixi` 目前为 `Unimplemented` 占位。
前置：P0 的"补丁版 rosdep ROS 包映射"与"真实 ARM pixi-pack"两项核验。

- [ ] 从清单生成 `pixi.toml`（RoboStack channel、ROS 发行版、任务定义）。
- [ ] 锁定委托给 `pixi.lock`；`rospm lock` 只记录其路径与 sha256（决策 #4）。
- [ ] rosdep 切换到 RoboStack 的 yaml 源：独立 `ROSDEP_SOURCE_PATH` / `ROS_HOME`，`--os=conda:linux`，不覆盖 native 的全局源。
- [ ] 探测并固定具备 ROS 包 conda 映射能力的 rosdep 版本/构建；安装交给 pixi。
- [ ] `deps` 子命令在此提供者下输出 conda 包名；`skip_keys` 在解析前自行过滤。
- [ ] 用第二个实现校正 `Provider` trait，去掉 P1 中的 apt 特有假设。
- **产出**：同一份清单，切换 `provider = "pixi"` 后在无 sudo 的机器上完成 apply/build。

---

## Phase 4 — Docker 提供者（约 2–3 周）

状态：**未开始**。`ProviderKind::Docker` 目前为 `Unimplemented` 占位。可与 P3 并行，都依赖 P2 的 Provider 抽象。

- [ ] 从清单生成 `Dockerfile`（分层：底包 → rosdep 依赖 → 源码 → 编译）、`compose.yaml`、`devcontainer.json`。
- [ ] 锁定记录 image digest；`apply` 即 build，`shell` 即 `exec`。
- [ ] 用 `buildx` 支持 `--platform linux/arm64`，为 P5 铺路。
- **产出**：`rospm apply` 直接产出可运行的镜像，`rospm shell` 进入容器且 ROS 环境就位。

---

## Phase 5 — aarch64 与平台覆盖（约 3 周）

状态：**未开始**。清单的 `[target.<platform>]` 覆盖/替换/排除已在 schema 与 `check --target` 中实现，但仓库索引查询与映射表未做。

- [x] 清单 `[target.<arch>]` 段：覆盖、替换、排除（schema + `effective_apt` 合并）。
- [ ] `rospm check --target aarch64`：查询 apt / conda 仓库索引，报告目标架构缺失的包。
- [ ] 内置可更新的替换映射表，与用户声明合并，用户声明优先。
- [ ] CI 增加 aarch64 集成矩阵（GitHub ARM runner，缺失时 QEMU）。
- **产出**：x86 上生成的锁文件，在 Raspberry Pi / Jetson 上 `apply --locked` 成功，缺失包有明确替代或明确报错。

---

## Phase 6 — 离线导出与导入（约 3–4 周）

状态：**未开始**。`export` / `import` 返回 NotImplemented(P6)；`apply --offline` 同样。

- [ ] 定义 bundle 格式：`manifest + lock + sources/ + payload/ + SHA256SUMS`（spec §5，决策 #10）。
- [ ] native：`apt-get download` 求闭包，`dpkg-scanpackages` 生成本地仓库；`dpkg --add-architecture` 跨架构下载。
- [ ] pixi：调用 `pixi-pack --platform`。
- [ ] docker：`docker save` 指定平台镜像。
- [ ] `rospm export --target aarch64` / `rospm import`；导入后走 `apply --locked --offline`。
- **产出**：联网 x86 机器导出，断网 arm64 机器导入并成功编译运行。

---

## Phase 7 — 编辑器集成与打磨（约 2 周）

状态：**未开始**。`editor` 返回 NotImplemented(P7)。

- [ ] 生成 `.vscode/settings.json`、`c_cpp_properties.json`、`launch.json`。
- [ ] CLion 走 `compile_commands.json`。
- [ ] devcontainer 复用 P4 产物。
- [ ] shell 补全、`doctor` 扩展自检、错误信息润色、文档站。
- [ ] 1.0 发布，冻结 manifest / lock schema v1。

---

## Phase 8 — GUI（选装，独立排期）

状态：**未开始**。

- [ ] Tauri 或 egui，只调用与 CLI 相同的库层（`rospm-core`），不另写业务逻辑。
- [ ] 功能：清单编辑、check / verify 结果可视化、导出向导。
