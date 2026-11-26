# rospm 分阶段实施清单

> 记录当前及后续阶段任务，状态更新日期：2026-11-26。
> 本文为阶段路线图；架构决策、格式与命令语义见 [spec.md](spec.md)。

图例：`[x]` 完成 · `[~]` 部分完成 · `[ ]` 未开始。
每个阶段末尾的“产出”是进入下一阶段的门槛。

## 长期测试记录与早期阶段剩余验收

已清理 Phase 0–2 的阶段实施说明，长期实测证据继续保留在 [P0 外部核验](p0-external-validation.md)、[P1 native 验收](p1-native-validation.md)、[P2 锁定重放](p2-locked-validation.md)。以下时间点记录原样保留，供后续跨机器、间隔复测比较。

- [x] `apply --locked` 的长期实测与相关修复：
  - [x] 时间点 1（2026-09-26，22.04 / Humble）非破坏性核验，脚本 `scripts/p2-locked-sandbox-check.sh`：真实 `lock` 发现并验签 `2026-08-07` / `20260808T004205Z`；隔离 apt 目录中两个快照的索引可下载；按快照合成的锁在空 dpkg 状态下完整解析 645 个包，`--allow-downgrades` 对 `python3-rosdep` 生效。
  - [x] 时间点 2（2026-10-07，Ubuntu 24.04 / Jazzy）用同一脚本复测：ROS 快照 `2026-09-11`、Ubuntu 快照 `20261007T141443Z`；锁定版本 1/1 在快照中，干净机模拟 669 包、当前主机模拟 126 包均成功。日志见 [p2-locked-validation.md](p2-locked-validation.md)。
  - [x] 在卸载恢复干净的 Ubuntu 24.04 虚拟机上真实执行新的 `apply --locked`（2026-10-07）：快照源安装 205 个锁定 apt 条目，随后 `check --strict`、`verify`、`deps check` 均退出 0，并再次 `uninstall --yes` 收尾。
- [ ] `env --shell fish`（原 P1 未完成项）。
- [ ] 两台机器、间隔数周执行真实 `apply --locked`，`verify` 报告一致（22.04 / Humble 与 24.04 / Jazzy 的已有记录见上；独立机器同锁比较与后续间隔复测仍待执行）。
- [ ] CI 增加 `lock` → 新容器 `apply --locked` → `verify` 场景。

---

## Phase 3 — aarch64 与平台覆盖

状态：**开发阶段预设支持已完成，原生 ARM 实机验收由用户在发布后完成**。目标仓库索引查询已完成并通过回归验证；内置替换映射表已完成；已搭建 Docker + QEMU 环境并通过 aarch64 集成验收。2026-10-27 原生 ARM runner 的 workspace 测试与 release 构建已通过；开发阶段以预设支持、QEMU 集成验收及双架构 CI 为边界，不承诺原生 ARM 实机闭环已验证。

- [ ] 发布后由用户在原生 ARM 设备上验收锁定还原、编译和节点运行，反馈环境信息与日志（发布后跟踪项，不作为开发完成或 1.0 发布门槛）。
- **产出**：开发阶段已验证 x86 最终生成的多平台锁在 QEMU aarch64 上 `apply --locked`、构建和节点运行成功，缺失包有明确替代或明确报错。原生 ARM 实机验收留待发布后由用户完成；当前未验证 Raspberry Pi / Jetson 的硬件接口与驱动。

---

## Phase 4 — Release 与 schema v1

状态：**已完成 release**

- [ ] 基于 aarch64 预设支持、QEMU 与双架构 CI，以及 Phase 3.5 Conda 自动配置、一键还原的已有验收，完成发布回归、文档与示例清单整理；注明原生 ARM 实机验收由用户在发布后完成。
- **产出**：1.0 发布，native 提供者与 schema v1 作为稳定基线，提供 aarch64 预设支持与产物，并注明实机验收边界。

---

## Phase 4.5 — 完整 TUI 与独立任务执行

状态：**已完成（2026-11-26，按实测支持范围）**。总体验收见 [P4.5.4 记录](p454-acceptance-20261126.md)。

目标：提供“环境概览与差异 → 审阅计划 → 执行 → 查看日志与诊断 → 处理失败”的完整终端工作流，支持键盘与鼠标、本机终端与 SSH。CLI 保留脚本 / CI 入口，TUI 复用同一业务逻辑、校验、确认与诊断，不通过解析 CLI 的人类可读输出判断任务状态。

架构边界：`rospm-core` 保持业务库；独立、按需启动的用户级 worker 持有任务与子进程，通过本机 Unix socket 与前端通信，可复用同一可执行文件的内部入口。TUI 关闭、卡死或崩溃不等于取消任务；worker 在任务结束并保存结果后退出。首期不引入常驻 daemon、远程网络协议、内嵌终端模拟器或任意步骤自动续跑。SSH 用户登录目标机器后在该机器运行 TUI；只有需要开机恢复、无人登录调度或长期共享任务时，再评估 systemd 用户服务 / daemon。

### 4.5.1 统一执行与交互接口

- **产出**：CLI 通过共享接口完成原有操作并通过回归；执行状态与交互请求可供 TUI 消费，不依赖终端文本解析。

### 4.5.2 按需 worker 与任务重连

- **产出**：TUI 卡死或被杀不影响正在执行的任务；支持范围内 SSH 断开后可重新连接并查看准确状态、日志与结果，无重复执行、管道阻塞或无界缓存。

### 4.5.3 TUI 工作流、权限与 shell

- **产出**：键盘与鼠标均可完成从计划审阅到失败处理的闭环；sudo 与 shell 不破坏界面或扩大项目操作权限，CLI / TUI 的业务结果一致。

### 4.5.4 集成验收与发布收尾

验收结论：**五项通过**。既有协议、故障、sudo 与 B1/B2 证据直接复用；本轮补齐真实 apt/dpkg 取消、远端 Humble 191 包安装及卸载、GNOME / SSH / tmux / 受限链路终端矩阵与文档同步。逐项证据及支持边界见 [总体验收记录](p454-acceptance-20261126.md)。

- [x] 执行接口与协议测试覆盖状态迁移、确认绑定、重复提交防护、工作区互斥、取消进程组、客户端断连与重连、最终结果持久化；CLI 原有行为与中英文 / JSON 输出回归通过。
- [x] 真实故障验收：暂停 / 杀死 TUI、大量 stdout / stderr、慢客户端、日志写入失败、worker 异常退出、SSH 断开；验证内存有界、子进程不因 UI 阻塞、结果不误报、重连不重复执行。
- [x] 在隔离环境验收系统安装中的退出 / 取消、sudo 认证失败与过期、等待确认时断连、清理目录后重试，确保不默认同意破坏性操作、不自动重放中断任务。
- [x] 制定并实测终端矩阵：常见本机终端、SSH、tmux，覆盖中文、鼠标、窄窗口、缩放、低带宽 / 高延迟；测试 shell 中运行交互程序、Ctrl-C 及退出后的界面恢复，记录实际支持范围与限制。
- [x] 同步命令帮助、README、spec、决策与路线图；说明任务日志位置及保留规则、退出 / 取消区别、提权方式、SSH 存活条件和故障恢复边界。
- **产出**：完整 TUI 可用于日常 native / Conda 工作流，任务与界面生命周期分离；支持范围有实测记录，CLI 自动化行为保持兼容。常驻 daemon、内嵌终端与主机重启后自动续跑继续独立评估，不作为本阶段门槛。

---

## Phase 5 — 离线导出与导入

状态：**未开始**。`export` / `import` 返回 NotImplemented(P5)；`apply --offline` 同样。

- [ ] 定义 bundle 格式：`manifest + lock + sources/ + payload/ + SHA256SUMS`（spec §5，决策 #10）。
- [ ] native：`apt-get download` 求闭包，`dpkg-scanpackages` 生成本地仓库；`dpkg --add-architecture` 跨架构下载。
- [ ] pixi：调用 `pixi-pack --platform`。
- [ ] docker：`docker save` 指定平台镜像。
- [ ] `rospm export --target aarch64` / `rospm import`；导入后走 `apply --locked --offline`。
- **产出**：联网 x86 机器导出，断网 arm64 机器导入并成功编译运行。

---

## Phase 6 — 编辑器集成与打磨

状态：**未开始**。`editor` 返回 NotImplemented(P6)。

- [ ] 生成 `.vscode/settings.json`、`c_cpp_properties.json`、`launch.json`。
- [ ] CLion 走 `compile_commands.json`。
- [ ] devcontainer 复用未来 Docker 提供者产物。
- [ ] shell 补全、`doctor` 扩展自检、基于 Phase 3.6 扩展诊断规则与提示、文档站。

---

## Phase 7 — GUI（选装，独立排期）

状态：**未开始**。

- [ ] Tauri 或 egui，只调用与 CLI 相同的库层（`rospm-core`），不另写业务逻辑。
- [ ] 功能：清单编辑、check / verify 结果可视化、导出向导；复用 Phase 3.6 的结构化诊断，展示错误位置、解释、建议与原始日志。
- [ ] 构建环境冲突交互：CLI 已对 native 构建自动取消激活不可用的 Conda 环境（Phase 3.6“构建环境基准”）；GUI 在该规则仍无法解决（如旧 CMake 缓存引用该环境、或自动取消激活后仍缺模块）或构建报出同类错误时，让用户选择目标 Conda 环境（对应 `--conda-env` / `--conda-prefix`，可写回 `build.conda`）、确认清理所列 build / install 目录（CLI 已有交互式确认兜底，GUI 以对话框提供同一选项），或取消激活后重试编译。
- [ ] 运行与调试便利功能（只调用 `rospm-core` 与 ROS 自带 CLI / GUI 工具，不另写 ROS 通信栈，也不重做 rqt 已有的界面）：

  可行性评估（2026-10-21，Humble 本机核对）：整体可行，且大部分已有 ROS 原生实现，rospm 的价值在于“在项目环境中一键打开、保存为项目配置、显示等价命令”。分工如下：

  | 需求 | 复用的原生组件 | rospm 自己做的部分 |
  |---|---|---|
  | 运行图浏览 | `rqt_graph`；轻量列表用 `ros2 node / topic / service / action list -t` | 在项目环境中启动、手动或低频刷新列表 |
  | 话题查看 | `rqt_topic`（频率、带宽、字段树）；`ros2 topic echo --once` / `hz` / `bw` | 无 |
  | 话题发布 | `rqt_publisher`；`ros2 topic pub --once` / `-r` / `-t` | 收藏时用 `ros2 interface proto <类型>` 预填 YAML |
  | 服务调用 | `rqt_service_caller`；`ros2 service call` | 同上 |
  | 动作调用 | 无 rqt 插件（`rqt_action` 只浏览类型）；`ros2 action send_goal -f`，SIGINT 时取消目标 | 启动 / 停止子进程并显示反馈与结果 |
  | 参数 | `rqt_reconfigure`；`ros2 param dump` / `load` | 导出 / 导入按钮 |
  | 日志 | `rqt_console` | 无 |
  | 启动节点 / launch | `ros2 pkg executables`、`ros2 launch -s`（列出 launch 参数）、`ros2 run` / `ros2 launch` | 运行配置、启动 / 停止 / 重启、输出与诊断（ROS 没有对应工具） |

  已知约束：
  - 默认底包 `ros-base` 不含 rqt（rqt 随 `desktop` 或 `ros-<distro>-rqt-common-plugins` 安装；本机 Humble ros-base 即无 rqt）。GUI 检测缺失时提示改用 `desktop` 或把该包加入 `dependencies.apt` 后执行 `apply`，不自动安装。
  - rqt 依赖 apt 版 Python / Qt，在受管 Conda 环境中运行很可能因 ABI 冲突失败：rqt 与 `ros2` 查询命令只加载 ROS 与 workspace 环境；只有项目节点（`ros2 run` / `ros2 launch`）在 Conda 环境中运行。
  - `ros2 daemon`（Humble ros2cli 源码核对）：只有 `ros2 … list` 等查询命令会在没有 daemon 时自动拉起它，节点、`ros2 run` / `ros2 launch` 不会；daemon 闲置 2 小时（`--timeout` 默认值，每次请求重新计时）后自行退出。处理规则：
    - 已有的 daemon 不是 rospm 启动的，直接复用，不违反“不常驻未声明的进程”，也不停止它。
    - 首次查询前用 `ros2 daemon status` 检查；原本没有、由 GUI 的查询拉起的 daemon 算 GUI 自己的，在进程列表中显示，退出 GUI 时 `ros2 daemon stop`。误停他人正在用的 daemon 影响很小，下次查询会自动重新拉起；GUI 崩溃遗留的由闲置超时清理。不自定义更短的超时（需绕过 `ros2 daemon start` 调用内部入口）。
    - 查询命令只按 `127.0.0.1` 和 `ROS_DOMAIN_ID` 推出的端口连接 daemon：在不同 `RMW_IMPLEMENTATION`、`ROS_LOCALHOST_ONLY` 或另一发行版环境下启动的已有 daemon 也会被复用，结果可能与项目环境不一致。GUI 显示 daemon 状态，环境不一致时提示改用 `--no-daemon` 或经用户确认后重启 daemon，不自动处理。
  - 每次调用 `ros2` 命令都要启动 Python（约 0.5–1 秒），列表只按需或低频刷新；按节点汇总发布 / 订阅关系需要逐节点执行 `ros2 node info`，开销大，交给 `rqt_graph`。
  - 运行配置与收藏若写入 `rospm.toml`，每次保存都会改变清单哈希而使 `rospm.lock` 过期；应放在独立的项目文件中（如 `rospm.run.toml`，可纳入版本控制），格式在实现时定，不进入 Phase 4 冻结的 schema v1。

  - [ ] 包与节点启动：列出 workspace 中的可执行文件（`ros2 pkg executables`）与 launch 文件，`ros2 launch -s` 读取可配置参数；以 `ros2 run` / `ros2 launch` 启动，环境与 `rospm env` / `shell` 一致（含受管 Conda 环境）；节点参数与 launch 参数可编辑并保存为运行配置；实时显示输出，可停止 / 重启，接入 Phase 3.6 的日志采集与诊断。为保持 GUI 只是 CLI 的薄壳，运行配置同时提供 CLI 入口（拟 `rospm run <名>`）。
  - [ ] ROS 工具入口：在项目环境中启动 `rqt_graph`、`rqt_topic`、`rqt_publisher`、`rqt_service_caller`、`rqt_reconfigure`、`rqt_console`，缺失时按上面的约束提示；另提供基于 `ros2 … list -t` 的轻量节点 / 话题 / 服务 / 动作列表（含类型、按命名空间分组），不重复实现关系图。
  - [ ] 动作调用：用 `ros2 action send_goal -f` 发送目标，显示反馈与结果，停止时发送 SIGINT 以取消目标。
  - [ ] 参数导出 / 导入：`ros2 param dump` / `load`，查看与修改交给 `rqt_reconfigure`。
  - [ ] 常用操作收藏：把一次发布、服务 / 动作调用或启动保存为运行配置文件中的快捷操作（YAML 内容由 `ros2 interface proto` 预填后编辑），一键重放，重放即执行对应的 `ros2` 命令。
  - [ ] 安全约束：所有操作显示等价的 CLI 命令；发布 / 调用前标明目标名称、类型与生效的 `ROS_DOMAIN_ID`（及 `ROS_LOCALHOST_ONLY` 等）并需确认，避免误操作真实机器人；按频率发布、启动的节点和 GUI 自己拉起的 daemon 都在界面上列出并可停止，关闭 GUI 时只结束自己启动的进程。
---

## Phase 8 — Pixi 提供者

状态：**未开始**。`ProviderKind::Pixi` 仍为 `Unimplemented` 占位；`pixi.toml` 生成的核心库（`rospm-core::pixi`）已合并，尚未接入 CLI。
前置：Phase 3 的 aarch64 开发阶段预设支持、Phase 4 的 1.0 release 与 Phase 7 的 GUI；P0 的"补丁版 rosdep ROS 包映射"与"真实 ARM pixi-pack"两项核验。

- [~] 从清单生成 `pixi.toml`（RoboStack channel、ROS 发行版、任务定义）。
  - [x] 核心库 `rospm-core::pixi`：`generate` 输出 RoboStack channel、`linux-64` / `linux-aarch64` 平台、底包与构建工具、`[activation.env]`、按叠加顺序的 `build-<ws>` 任务与汇总 `build` 任务；`read_summary` 读取现有 `pixi.toml`（含 `[project]` 与表形式 channel）。2026-09-26 已合并到 main。
  - [ ] 叠加：上层 workspace 的任务需要先加载下层 `install/setup`，pixi 任务 shell 不支持 `source`，方案待定。
  - [ ] CLI 接入（`init` / `apply` 写文件、已有非 rospm 生成的 `pixi.toml` 时不覆盖）。
- [ ] 锁定委托给 `pixi.lock`；`rospm lock` 只记录其路径与 sha256（决策 #4）。
- [ ] rosdep 切换到 RoboStack 的 yaml 源：独立 `ROSDEP_SOURCE_PATH` / `ROS_HOME`，`--os=conda:linux`，不覆盖 native 的全局源。
- [ ] 探测并固定具备 ROS 包 conda 映射能力的 rosdep 版本/构建；安装交给 pixi。
- [ ] `deps` 子命令在此提供者下输出 conda 包名；`skip_keys` 在解析前自行过滤。
- [ ] 用第二个实现校正 `Provider` trait，去掉 P1 中的 apt 特有假设。
- **产出**：同一份清单，切换 `provider = "pixi"` 后在无 sudo 的机器上完成 apply/build。

---

## Phase 9 — Docker 提供者（约 2–3 周）

状态：**未开始**。`ProviderKind::Docker` 目前为 `Unimplemented` 占位，前置为 Phase 3 的 aarch64 开发阶段预设支持、Phase 4 的 1.0 release 与 Phase 7 的 GUI。

- [ ] 从清单生成 `Dockerfile`（分层：底包 → rosdep 依赖 → 源码 → 编译）、`compose.yaml`、`devcontainer.json`。
- [ ] 锁定记录 image digest；`apply` 即 build，`shell` 即 `exec`。
- [ ] 用 `buildx` 支持 `--platform linux/arm64`，复用 Phase 3 的 QEMU 测试环境验证跨架构镜像。
- **产出**：`rospm apply` 直接产出可运行的镜像，`rospm shell` 进入容器且 ROS 环境就位。
