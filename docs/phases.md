# rospm 分阶段实施清单

> 记录当前及后续阶段任务，状态更新日期：2026-10-09。
> 总览见 [ROADMAP.md](../ROADMAP.md)；决策见 [decisions.md](decisions.md)；格式与命令语义见 [spec.md](spec.md)。

图例：`[x]` 完成 · `[~]` 部分完成 · `[ ]` 未开始。
每个阶段末尾的“产出”是进入下一阶段的门槛。

## 长期测试记录与早期阶段剩余验收

已清理 Phase 0–2 的阶段实施说明，长期实测证据继续保留在 [P0 外部核验](p0-external-validation.md)、[P1 native 验收](p1-native-validation.md)、[P2 锁定重放](p2-locked-validation.md)。以下时间点记录原样保留，供后续跨机器、间隔复测比较。

- [x] `apply --locked` 的长期实测与相关修复：
  - [x] 时间点 1（2026-09-26，22.04 / Humble）非破坏性核验，脚本 `scripts/p2-locked-sandbox-check.sh`：真实 `lock` 发现并验签 `2026-08-07` / `20260808T004205Z`；隔离 apt 目录中两个快照的索引可下载；按快照合成的锁在空 dpkg 状态下完整解析 645 个包，`--allow-downgrades` 对 `python3-rosdep` 生效。
  - [x] 修复：写 `rospm-ubuntu-snapshot.list` 后缺少 `apt-get update`，锁定安装原本看不到 Ubuntu 快照索引（`tests/plan.rs` 加断言）。
  - [x] 真实闭环（2026-09-26，本机，用户同意实装）：实时 `apply` → `build` / 运行节点 → `lock` → `verify` → `uninstall`，已装包列表与基线完全一致；快照锁真实 `apply --locked` 成功（212 个包、`python3-rosdep` 降级、`git checkout --detach`），`verify` / `deps check` / `check --strict` 全部就绪，节点用锁定的 rclcpp 16.0.19 编译运行，重新 `lock` 与输入逐条一致，`uninstall` 后只剩按设计保留的工具版本差异。
  - [x] 修复：keyserver 返回的快照密钥为 armor 格式，原样写入 `.gpg` 导致 apt 报 `NO_PUBKEY`；现在先 `gpg --dearmor`，探测识别 armor 格式的 keyring 并重装。
  - [x] 修复：`uninstall` 在 `ros2.list` 已不存在时漏删快照 keyring 与 `rospm-ubuntu-snapshot.list`。
  - [x] 修复（2026-09-27）：失败的 `apply --locked` 留下坏源后，计划起始的 `apt-get update` 使重跑失败，只能 `uninstall` 恢复。现在起始 update 之前先删除本次会重写的 rospm 自有源文件（`ros2.list`、`rospm-ubuntu-snapshot.list`），外来文件与无需重写的文件不动；`tests/plan.rs` 用"armor keyring + 旧 Ubuntu 快照"的残留状态断言删除先于首个 update、两文件随后重写。
  - [x] 修复（2026-09-27）：`tests/apply_locked.rs` 依赖主机 keyring 状态。现在只在主机没有可用（二进制）快照 keyring 时断言计划含密钥指纹；快照 URL 与 Ubuntu 快照文件的断言对"写入"与"已配置"两种输出都成立。两种主机状态的完整断言由 `tests/plan.rs` 在伪造主机上覆盖。
  - [x] 设计问题（2026-09-27 解决）：`lock` 记录的版本可能不在所选快照里（实时 ROS 源底包构建于 2026-09-08，最新快照为 2026-08-07；Ubuntu 快照由 ROS `Release` 日期换算，晚于它的 Ubuntu 更新同样缺失）。现在 `lock` 下载快照 `Packages.gz`（系统 `gzip -dc` 解压，不加依赖）逐包核对：ROS 快照取最新三个日期中第一个含全部锁定 `ros-<distro>-*` 版本的，都不含则不记录并列出缺失版本；Ubuntu 快照时间戳改取本机 `/var/lib/dpkg/status` 修改时间（最后一次装包），取不到才退回 ROS `Release` 日期；其余包在两个快照中都找不到的列出。本机实测（22.04，未装 ROS）：真实 `lock` 下载并核对 ROS 与 Ubuntu 索引，4 个包全部命中，Ubuntu 快照 `20260926T105514Z` 验签通过，约 11 秒。缺版本的分支由罐装服务器测试覆盖。
  - [x] 设计问题（2026-09-27 解决）：锁文件只记录顶层 Ubuntu 包，系统库比快照新的主机上 `curl=<旧版本>` 因依赖精确版本而使整条安装失败（模拟退出 100）。现在 ROS 层（`ros-<distro>-*`、底包）与三个工具包仍精确安装、可降级；其余 Ubuntu 包本机已装且更新时保持不变并在计划中列出，`verify` 报 `info`（新增 `debver` 模块按 dpkg 规则比较版本，用例与真实 `dpkg --compare-versions` 对照一致）。本机 `apply --locked --dry-run` 实测：锁定 `curl …1ubuntu1.25`、本机 `…1ubuntu1.29` 时计划只保留说明、不再点名 `curl=`。
  - [x] 修复（2026-09-27）：锁文件缺底包时 `verify` 仍报就绪。现在本机平台没有底包记录（含完全没有 apt 记录）时底包一行 `MISSING`、退出 1，与 `apply --locked` 拒绝该锁一致；核心与 CLI 测试覆盖。
  - [x] 时间点 2（2026-10-07，Ubuntu 24.04 / Jazzy）用同一脚本复测：ROS 快照 `2026-09-11`、Ubuntu 快照 `20261007T141443Z`；锁定版本 1/1 在快照中，干净机模拟 669 包、当前主机模拟 126 包均成功。日志见 [p2-locked-validation.md](p2-locked-validation.md)。
  - [x] 在卸载恢复干净的 Ubuntu 24.04 虚拟机上真实执行新的 `apply --locked`（2026-10-07）：快照源安装 205 个锁定 apt 条目，随后 `check --strict`、`verify`、`deps check` 均退出 0，并再次 `uninstall --yes` 收尾。

- [ ] 在 GitHub 实际跑通 CI（含 `ubuntu-24.04-arm` runner）。
- [ ] `env --shell fish`（原 P1 未完成项）。
- [ ] 两台机器、间隔数周执行真实 `apply --locked`，`verify` 报告一致（22.04 / Humble 与 24.04 / Jazzy 的已有记录见上；独立机器同锁比较与后续间隔复测仍待执行）。
- [ ] CI 增加 `lock` → 新容器 `apply --locked` → `verify` 场景。

---

## Phase 3 — aarch64 与平台覆盖（约 3 周）

状态：**进行中**。目标仓库索引查询已完成并通过回归验证（2026-10-09）；替换映射表与 aarch64 集成验收尚未开始。

- [x] `rospm check --target aarch64`：查询 apt / conda 仓库索引，报告目标架构缺失的包（2026-10-09）。覆盖 apt 平台合并、Ubuntu ports 与更新/安全组件、conda 目标子目录与 `noarch`；失败索引报告 `unknown`，`--strict` 返回非零。单测、CLI 本地 HTTP 集成测试与真实网络查询证据见 [p3-repository-validation.md](p3-repository-validation.md)。
- [ ] 内置可更新的替换映射表，与用户声明合并，用户声明优先。
- [ ] 在 Docker 容器内搭建 QEMU aarch64 环境，运行 native 提供者的 `check` / `apply --locked` / `build` / `ros2 run` 集成测试，验证 aarch64 的可用性与兼容性；优先使用 GitHub ARM runner，缺失时以 QEMU 作为稳定回退。
- **产出**：x86 上生成的锁文件，在 Raspberry Pi / Jetson 上 `apply --locked` 成功，缺失包有明确替代或明确报错。

---

## Phase 4 — Release 与 schema v1（约 1–2 周）

状态：**未开始**。

- [ ] 完成 aarch64 验收后的回归测试、文档与示例清单整理。
- [ ] 冻结 manifest / lock schema v1，整理迁移说明与兼容性声明。
- [ ] 构建并发布 1.0 版本及 aarch64 产物。
- **产出**：1.0 发布，native 提供者、aarch64 目标与 schema v1 作为稳定基线。

---

## Phase 5 — 离线导出与导入（约 3–4 周）

状态：**未开始**。`export` / `import` 返回 NotImplemented(P5)；`apply --offline` 同样。

- [ ] 定义 bundle 格式：`manifest + lock + sources/ + payload/ + SHA256SUMS`（spec §5，决策 #10）。
- [ ] native：`apt-get download` 求闭包，`dpkg-scanpackages` 生成本地仓库；`dpkg --add-architecture` 跨架构下载。
- [ ] pixi：调用 `pixi-pack --platform`。
- [ ] docker：`docker save` 指定平台镜像。
- [ ] `rospm export --target aarch64` / `rospm import`；导入后走 `apply --locked --offline`。
- **产出**：联网 x86 机器导出，断网 arm64 机器导入并成功编译运行。

---

## Phase 6 — 编辑器集成与打磨（约 2 周）

状态：**未开始**。`editor` 返回 NotImplemented(P6)。

- [ ] 生成 `.vscode/settings.json`、`c_cpp_properties.json`、`launch.json`。
- [ ] CLion 走 `compile_commands.json`。
- [ ] devcontainer 复用未来 Docker 提供者产物。
- [ ] shell 补全、`doctor` 扩展自检、错误信息润色、文档站。

---

## Phase 7 — GUI（选装，独立排期）

状态：**未开始**。

- [ ] Tauri 或 egui，只调用与 CLI 相同的库层（`rospm-core`），不另写业务逻辑。
- [ ] 功能：清单编辑、check / verify 结果可视化、导出向导。

---

## Phase 8 — Pixi 提供者（约 3 周）

状态：**进行中**（2026-09-26）。`ProviderKind::Pixi` 仍为 `Unimplemented` 占位；`pixi.toml` 生成的核心库（`rospm-core::pixi`）已合并，尚未接入 CLI。
前置：Phase 3 的 aarch64 验收、Phase 4 的 1.0 release 与 Phase 7 的 GUI；P0 的"补丁版 rosdep ROS 包映射"与"真实 ARM pixi-pack"两项核验。

- [~] 从清单生成 `pixi.toml`（RoboStack channel、ROS 发行版、任务定义）。
  - [x] 核心库 `rospm-core::pixi`（2026-09-26，codex 实现、主会话 review 并补测试）：`generate` 输出 RoboStack channel、`linux-64` / `linux-aarch64` 平台、底包与构建工具、`[activation.env]`、按叠加顺序的 `build-<ws>` 任务与汇总 `build` 任务；`read_summary` 读取现有 `pixi.toml`（含 `[project]` 与表形式 channel）。2026-09-26 已合并到 main。
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

状态：**未开始**。`ProviderKind::Docker` 目前为 `Unimplemented` 占位，前置为 Phase 3 的 aarch64 验收、Phase 4 的 1.0 release 与 Phase 7 的 GUI。

- [ ] 从清单生成 `Dockerfile`（分层：底包 → rosdep 依赖 → 源码 → 编译）、`compose.yaml`、`devcontainer.json`。
- [ ] 锁定记录 image digest；`apply` 即 build，`shell` 即 `exec`。
- [ ] 用 `buildx` 支持 `--platform linux/arm64`，复用 Phase 3 的 QEMU 测试环境验证跨架构镜像。
- **产出**：`rospm apply` 直接产出可运行的镜像，`rospm shell` 进入容器且 ROS 环境就位。
