# rospm 分阶段实施清单

> 从 `ROADMAP.md` §5 拆出，记录每个阶段的具体任务与完成状态。状态更新日期：2026-09-26。
> 总览、可行性与风险分析仍在 [ROADMAP.md](../ROADMAP.md)；决策见 [decisions.md](decisions.md)；格式与命令语义见 [spec.md](spec.md)。

图例：`[x]` 完成 · `[~]` 部分完成（子项说明） · `[ ]` 未开始。
每个阶段末尾的"产出"是进入下一阶段的门槛，不满足不进入下一阶段。

---

## Phase 0 — 规格与骨架（约 1–2 周）

状态：**基本完成**。本地可做的验证已全部完成（2026-09-25）；仅剩 GitHub 上实跑 CI 与人工 review。

- [x] 补齐 ROADMAP §3 所列 13 项决策，写成 `docs/decisions.md` 与 `docs/spec.md`（2026-09-15）。
- [x] 定义 manifest v0 schema 与 lock v0 schema，附示例（`examples/minimal`，spec §2.1 / §3.1）。
- [x] Rust 工程骨架：`clap` CLI、`Provider` trait、`Scm` trait（`Vcstool`）、日志、错误类型、`Plan` + `--dry-run` 基础设施。
  - [x] `rospm-core`：`manifest` / `lock` / `plan` / `platform` / `provider` / `scm` 模块。
  - [x] 单元测试：清单解析与校验、拓扑排序、平台覆盖合并、TOML 往返、native 计划生成（`cargo test --workspace` 全绿）。
- [~] CI：fmt + clippy + 单元测试（x86_64 / arm64）+ Ubuntu 22.04 / 24.04 容器冒烟。
  - [x] `.github/workflows/ci.yml` 已编写。
  - [x] 本地逐条复现 lint / test / release build / 22.04 smoke 全部通过（2026-09-25，见 [p0-external-validation.md §5](p0-external-validation.md)）。
  - [ ] 在 GitHub 上实际跑通一次（含 `ubuntu-24.04-arm` runner）。
- [x] 验证 spec §8 列出的 4 项外部事实（2026-09-25 全部有本地实测证据）。实测过程与证据见 [p0-external-validation.md](p0-external-validation.md)。
  - [x] Ubuntu 22.04 快照：APT 2.4.11+ 已回移植；`--snapshot` 与直接 URL 两种方式在 jammy/main/amd64 上实测通过。
  - [x] ROS 快照：目录布局 `snapshots.ros.org/<distro>/<date>/ubuntu/…`，日期不定期；索引大小与 SHA256 与 Release 一致；Rolling 当前 404。
  - [x] ROS 快照 `Release.gpg` 签名密钥来源核验：密钥 `4B63…CBF125EA` 与 `packages.ros.org` 的 `ros.key` 不同，来源为官方 wiki SnapshotRepository 页 + OSRF 官方 Dockerfile，从 `keyserver.ubuntu.com` 取得后 `gpgv` 验证 Release.gpg / InRelease 通过，有效期至 2027-06-01。P2 必须用 `signed-by=` 引用该 keyring，禁止 `trusted=yes`。
  - [x] RoboStack rosdep 源链、`--os=conda:linux` 系统键映射、`skip_keys` 只作用于 install/check 的语义。
  - [x] conda-forge 补丁版 rosdep（0.26.0 build 1）对 ROS 包（`rclcpp`）的 conda 映射对照：`--os=conda:linux` 解析为 `ros-humble-rclcpp`，系统键与上游 master 一致。附带发现：其 `check`/`install` 只兼容 micromamba 1.x（2.x 的 `list --json` 格式不同），P3 不应依赖 rosdep 的 conda 检测。
  - [x] pixi 0.81.0 完整下载并校验（SHA256 与官方 .sha256 一致），`pixi lock` + `pixi-pack --platform linux-aarch64` 在 x86 上真实打包成功，归档内 34 个包的 SHA256 与 `pixi.lock` 逐一匹配。ARM 主机实际解包运行移交 P3/P5。
- [x] spec 与示例清单 review。（仅限当前阶段，后续如有变更仍需重新 review ）
- **产出**：`rospm --help` 可运行 ✅。

> 骨架已超前实现 P1 的部分内容（见下），因此 P1 从"补探测与实跑"开始，而不是从零开始。

---

## Phase 1 — MVP：native 提供者的描述与重放（约 3–4 周）

范围：x86_64、Ubuntu 22.04/24.04、Humble/Jazzy、native(apt) 提供者、git 源码、colcon。
状态：**进行中**。`init` / `check` 已完成（2026-09-25）；`apply` 的 ROS 安装 bootstrap 与 `uninstall` 计划已落地（2026-09-26）；`apply` / `build` / `uninstall` 仍缺真实机器验证。

### `rospm init`
- [x] 生成清单模板（`templates/rospm.toml`），已存在需 `--force`，模板自校验。
- [x] `--from-env`：best-effort 探测（决策 #3），探测不到的字段留默认值并标 `# TODO`，终端同步列出 `detected` / `TODO`；生成结果必须通过 `Manifest::parse`（2026-09-25，`rospm-core::introspect`）。
  - 探测来源：`$ROS_DISTRO` → `$AMENT_PREFIX_PATH` 中的 `/opt/ros/<d>` → `/opt/ros` 下唯一安装；`/etc/os-release`；dpkg 中 `ros-<d>-{desktop-full,desktop,ros-base}`；`ROS_*` / `RMW_*` 等白名单环境变量；`.vscode` / `.idea` / `.devcontainer`；`pixi.toml` 存在则切到 pixi 提供者并从其 channel / 依赖读发行版。
- [x] `--base ros-base|desktop`：选择底包元包（2026-09-26）。模板路径按模板发行版拼包名；`--from-env` 下以探测到的发行版拼包名并覆盖探测结果，探测不到发行版则标 `TODO`。清单字段 `ros_base` 仍为自由包名，`desktop-full` 可手改。
- [x] 从当前目录的 `.repos` / `package.xml` 反推 workspaces 与依赖（2026-09-25）。
  - 含 `src/` 的目录（根目录及两层子目录）为 workspace；`*.repos` 按 `src/` 下已存在的 checkout 匹配到 workspace，未匹配且只剩一个空 workspace 时配对，否则生成路径待定的 workspace；只含注释的 `.repos` 忽略。
  - 叠加顺序从 `$AMENT_PREFIX_PATH` 推导（兼容 colcon 默认的 isolated install），推不出时标 `TODO`。
  - `package.xml` 解析出的外部 rosdep 键只作为注释写入 `[dependencies]`（解析仍交给 rosdep）；`dependencies.apt` 无法反推，固定标 `TODO`。colcon mixins 不承诺还原（决策 #8）。
  - 已用 ros2/demos + ros2/examples（43 个 `package.xml`）与 nav2 的 `underlay.repos` 实测。

### `rospm check`
- [x] 清单校验、拓扑排序、平台覆盖合并、`--target <platform>`、`--json` 输出。
- [x] 环境探测（2026-09-25，`Provider::probe_env` + `rospm-core::probe`）：平台与 Ubuntu 版本、git / vcs / colcon / rosdep / apt-get 是否在 PATH、`/opt/ros/<distro>` 与底包 dpkg 状态、rosdep `init`（`20-default.list`）/ `update`（`$ROS_HOME/rosdep/sources.cache/index`）、平台覆盖后的额外 apt 包 dpkg 状态、每个 `.repos` 条目是否已 import 到 `src/`（git checkout 且 `origin` URL 一致，宽松比较 `.git` / ssh 写法）、`src/` 下 `package.xml` 数量、colcon defaults 文件、是否已 build。
- [x] ROS 安装前置条件探测（2026-09-26）：locale 是否 UTF-8、apt `universe` 是否启用（一行式与 deb822 两种源格式）、是否已有引用 `packages.ros.org/ros2` 的 apt 源文件。`doctor` 同步输出这三项。
- [x] 输出差异报告（期望 vs 实际），不修改系统。状态分 `ok` / `MISSING` / `MISMATCH` / `unknown`（探测工具缺失）/ `info`；默认只报告、退出码 0，`--strict` 在未就绪时退出 1（供 CI）；`--target` 与主机不同时跳过探测并注明；pixi / docker 提供者输出一行"P3 / P4 未实现"。

### `rospm apply`
- [x] native 计划生成：ROS apt 源 → 底包 → `vcs import` → `rosdep install` → 额外 apt；`--dry-run` 打印计划。
- [x] 安装 ROS 的 bootstrap（2026-09-26，spec §4.1，决策 #14）：主机 Ubuntu 版本与清单不符直接报错；按探测结果只补缺的步骤：`locales` + `locale-gen`/`update-locale`、`add-apt-repository universe`、下载 `ros.key` 到 keyring、写 `sources.list.d/ros2.list`（`--dry-run` 打印文件内容）、`apt-get update`。系统文件经 `sudo tee` 写入。CI 冒烟对 22.04 / 24.04 各用一份匹配的示例清单，并断言计划里含源文件写入。
- [x] `--locked` 在 P2 前明确降级告警；`--offline` 返回 NotImplemented(P6)。
- [ ] 非 dry-run 在真实机器上执行并处理失败（sudo 交互、apt 锁、rosdep 未初始化等）。
- [ ] `ros.key` 下载失败 / 密钥指纹核验（当前只 `curl -o`，未校验指纹）。

### `rospm uninstall`
- [x] 计划生成（2026-09-26，决策 #15）：用 dpkg 列出全部已装 `ros-<distro>-*` 并显式写进 `apt-get remove`，`autoremove`；删除 rospm 写的 `ros2.list` 与 keyring，`--keep-source`、`/opt/ros` 下还有其它发行版、或源文件非 rospm 所写（如 `ros2-apt-source`）时保留并说明；不卸 rosdep / vcstool / colcon；提示 `/opt/ros/<distro>` 可能残留。
- [x] 非 `--dry-run` 需交互确认或 `--yes`；stdin 非终端且无 `--yes` 拒绝执行；dpkg-query 不可用时报错而不猜。
- [ ] 真实机器上验证卸载后 `check` 报告与干净机器一致。

### `rospm build`
- [x] 封装 colcon，按叠加顺序 source underlay，注入清单中的 colcon 配置；`--dry-run`。
- [ ] 真实机器上验证 `[workspaces.<name>.colcon]` 各字段的实际效果。

### `rospm env` / `rospm shell`
- [x] `env --shell bash|zsh`；按 workspace 顺序 source，导出 `[env]`。
- [ ] `env --shell fish`（spec §4 已列出）。
- [x] `shell`：写入 `.rospm/env.sh` 与 `shellrc`，启动带前缀提示符的交互 bash。

### `rospm doctor`
- [x] 检测 git / vcs / colcon / rosdep / apt-get / dpkg-query / pixi / docker 与 `/opt/ros/*`，`--json`。
- [x] 输出 ROS apt 源、`universe`、locale 状态（2026-09-26）。

### 验收
- [ ] 在一台干净 22.04 虚拟机上，仅凭清单 + 源码完成 `apply && build && shell`，`ros2 run` 成功（`apply` 须从无 ROS apt 源起步）。
- [ ] 同样流程在 24.04 + Jazzy 上通过。
- [ ] `rospm uninstall --yes` 后 `rospm check` 对 ROS 各项回到 `MISSING`。

---

## Phase 1.5 — 输出本地化 i18n（约 1 周）

状态：**完成**（2026-09-26）。决策 #16，规则见 spec §4.2。插在 P1 与 P2 之间：P1 的命令集已稳定，现存面向用户的字符串约 150 条（CLI 输出 43、计划注释与探测报告 68、`init --from-env` 说明 32），回填成本可控；P2 起每个阶段都会新增大量字符串，晚做只会更贵。不阻塞 P1 的干净虚拟机验收，可并行。

- [x] 选库并落基础设施（2026-09-26）：`rust-i18n`，资源 `crates/rospm-cli/locales/{en,zh-CN}.yml` 编译期嵌入。`rospm-core` 新增 `message::Message`（原样文本或键 + 参数）与 `msg!` 宏；计划标题 / 注释、探测报告各列、`Error`、`init --from-env` 的 notes / todos / 草稿注释全部改为 `Message`，CLI 用 `set_renderer` 安装渲染函数，核心库不依赖 locale（未安装渲染器时显示 `key(arg=value)`）。
- [x] locale 选择（2026-09-26）：`--lang` > `ROSPM_LANG` > `LANGUAGE`（冒号列表，取首个 `zh*` / `en*`）> `LC_ALL` > `LC_MESSAGES` > `LANG`；`zh*` → `zh-CN`，其余（含未设置 / `C` / `POSIX`）→ `en`。在构建 clap 命令之前完成，`--help` 与自实现的 `help` 子命令、章节标题均本地化。单测覆盖 `zh_CN.UTF-8` / `zh_TW` / `C` / `POSIX` / 未设置 / `LANGUAGE=zh:en` / `LANGUAGE=en:zh` / `LC_ALL` 优先级 / `--lang` 与 `ROSPM_LANG` 覆盖。
- [x] 回填现有字符串（2026-09-26）：clap 说明与 `-h` / `-V` 说明、`init`（`detected` / `TODO` 行、模板与草稿注释）、`check` 表格（状态列、主题列、期望 / 实际）与总结、`doctor`、`apply` / `uninstall` / `build` 计划标题与注释、确认提示、清单校验与 `Error` 全部变体。`--json` 键名与枚举值保持英文，自由文本随语言。已知限制：clap 自身的解析错误信息与 `[OPTIONS]` / `<COMMAND>` 占位符仍为英文。
- [x] `init` 模板按语言选文件（`templates/rospm.{en,zh-CN}.toml`），`--base` 改为按行重写 `ros_base`；单测断言两份模板（含 `--base` 改写后）都通过 `Manifest::parse`。
- [x] 单测：两种语言键集合与占位符集合一致（`_test.*` 键只在 en 中，用于测缺键回退）；缺键回退英文、两边都缺时返回键名，不 panic。CLI 集成测试显式设 `LANG=C` / `LANG=zh_CN.UTF-8`，断言中文输出与退出码不变、`--json` 枚举值仍为英文。CI 冒烟增加 `LANG=zh_CN.UTF-8` 一轮（`--help` / `check` / `apply --dry-run` / 错误）。
- [x] `docs/spec.md` §4.2 补充实现说明，README 增加 "Language / 语言切换"。
- **产出**：`LANG=zh_CN.UTF-8` 下 `--help` / `check` / `apply --dry-run` / 错误信息全部为中文，`LANG=C` 下全部为英文。此后 review 把裸英文用户字符串视为缺陷。

---

## Phase 2 — 锁文件与依赖补全（约 3 周）

状态：**未开始**。`rospm.lock` 的数据结构与读取（`lock.rs`）已随骨架落地；`lock` / `verify` / `deps` 命令目前返回 NotImplemented(P2)。
前置：P0 的"ROS 快照签名密钥"核验必须先完成，否则 `apply --locked` 无法可信地切换源；P1.5 的 i18n 层必须先落地，本阶段新增字符串直接走该层。

### `rospm lock`
- [x] lock v0 schema 与解析（`Lock::parse`）。
- [ ] 源码层：`vcs export --exact` → commit SHA。
- [ ] apt 层：按平台记录已安装版本 + ROS / Ubuntu 快照日期。快照日期必须**发现**真实存在的日期并验证 suite、架构、签名，不能把请求日期直接拼进 URL。
- [ ] pip 层：`pip freeze` 结果只记录供 `verify` 对比（决策 #6）。
- [ ] 记录清单哈希；同一输入产生同一输出（幂等）。
- [ ] 锁文件读写往返测试。

### `rospm apply --locked`
- [ ] 把 sources.list 切到 `snapshots.ros.org` 与 `snapshot.ubuntu.com`（22.04 用 `--snapshot` 或直接 URL，保留 keyring 签名验证）。复用 P1 bootstrap 的源文件管理（同一 `ros2.list` 路径、`signed-by=` 换成快照 keyring），不另起一套。
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
