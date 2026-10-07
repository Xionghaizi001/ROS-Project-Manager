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
  - [x] conda-forge 补丁版 rosdep（0.26.0 build 1）对 ROS 包（`rclcpp`）的 conda 映射对照：`--os=conda:linux` 解析为 `ros-humble-rclcpp`，系统键与上游 master 一致。附带发现：其 `check`/`install` 只兼容 micromamba 1.x（2.x 的 `list --json` 格式不同），P8 不应依赖 rosdep 的 conda 检测。
  - [x] pixi 0.81.0 完整下载并校验（SHA256 与官方 .sha256 一致），`pixi lock` + `pixi-pack --platform linux-aarch64` 在 x86 上真实打包成功，归档内 34 个包的 SHA256 与 `pixi.lock` 逐一匹配。ARM 主机实际解包运行移交 P3/P8。
- [x] spec 与示例清单 review。（仅限当前阶段，后续如有变更仍需重新 review ）
- **产出**：`rospm --help` 可运行 ✅。

> 骨架已超前实现 P1 的部分内容（见下），因此 P1 从"补探测与实跑"开始，而不是从零开始。

---

## Phase 1 — MVP：native 提供者的描述与重放（约 3–4 周）

范围：x86_64、Ubuntu 22.04/24.04、Humble/Jazzy、native(apt) 提供者、git 源码、colcon。
状态：**22.04 / Humble 验收通过**（2026-09-26，见 [p1-native-validation.md](p1-native-validation.md)）。`init` / `check` 已完成（2026-09-25）；`apply` / `build` / `shell` / `uninstall` 已在干净的 Ubuntu 22.04 主机上实跑闭环。剩余：24.04 + Jazzy 同流程、`env --shell fish`。

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
- [x] 输出差异报告（期望 vs 实际），不修改系统。状态分 `ok` / `MISSING` / `MISMATCH` / `unknown`（探测工具缺失）/ `info`；默认只报告、退出码 0，`--strict` 在未就绪时退出 1（供 CI）；`--target` 与主机不同时跳过探测并注明；pixi / docker 提供者输出一行"P8 / P9 未实现"。

### `rospm apply`
- [x] native 计划生成：ROS apt 源 → 底包 → `vcs import` → `rosdep install` → 额外 apt；`--dry-run` 打印计划。
- [x] 安装 ROS 的 bootstrap（2026-09-26，spec §4.1，决策 #14）：主机 Ubuntu 版本与清单不符直接报错；按探测结果只补缺的步骤：`locales` + `locale-gen`/`update-locale`、`add-apt-repository universe`、下载 `ros.key` 到 keyring、写 `sources.list.d/ros2.list`（`--dry-run` 打印文件内容）、`apt-get update`。系统文件经 `sudo tee` 写入。CI 冒烟对 22.04 / 24.04 各用一份匹配的示例清单，并断言计划里含源文件写入。
- [x] `--locked` 在 P2 前明确降级告警；`--offline` 返回 NotImplemented(P5)。
- [x] 非 dry-run 在真实机器上执行并处理失败（2026-09-26）：干净 22.04 主机上从无 ROS apt 源起步，11 步全部成功；命令失败时以非零退出并打印失败命令（实测 `curl` 被 SIGTERM 中断的场景）；二次 `apply` 幂等（bootstrap 步骤按探测结果跳过，apt / vcs / rosdep 自身幂等）。sudo 免密与 `rosdep init` 未初始化两种情况已覆盖；apt 锁冲突与 sudo 交互式输密码未构造场景。
- [x] `ros.key` 下载与密钥指纹核验（2026-09-26）：先下到临时文件（`curl -f`、连接 / 单次超时、瞬时错误重试），`gpg --show-keys` 核对完整指纹 `C1CF…C654` 后才 `sudo install` 到 keyring；404、指纹不符、下载挂起都报错且不写 keyring。前置包加入 `gnupg`（ubuntu 容器镜像只有 `gpgv`）。三个需要网络的 `#[ignore]` 测试本机实跑通过；真实 `apply` 中该步骤在网络停滞时 3 分钟内有界失败、网络正常时通过指纹校验并安装。

- [x] 源镜像与网络测速（2026-09-26，决策 #17，spec §4.1.1）：内置 `tuna` / `ustc` / `aliyun`，`apply` 执行前并行测速并打表，官方不通时交互引导或报错提示 `--mirror`；`doctor` 展示同一张表。镜像 URL 布局与 rosdep 源列表已实测；`apply --mirror tuna` 在 GitHub 不通的主机上完整闭环（13 步、rosdep 走镜像），交互提示与非终端报错均实测（[p1-native-validation.md §7](p1-native-validation.md)）。

### `rospm uninstall`
- [x] 计划生成（2026-09-26，决策 #15）：用 dpkg 列出全部已装 `ros-<distro>-*` 并显式写进 `apt-get remove`，`autoremove`；删除 rospm 写的 `ros2.list` 与 keyring，`--keep-source`、`/opt/ros` 下还有其它发行版、或源文件非 rospm 所写（如 `ros2-apt-source`）时保留并说明；不卸 rosdep / vcstool / colcon；提示 `/opt/ros/<distro>` 可能残留。
- [x] 非 `--dry-run` 需交互确认或 `--yes`；stdin 非终端且无 `--yes` 拒绝执行；dpkg-query 不可用时报错而不猜。
- [x] 真实机器上验证卸载后 `check` 报告与干净机器一致（2026-09-26）：197 个 `ros-humble-*` 全部移除，`/opt/ros` 整目录消失，`ros2.list` 与 keyring 删除，`check` 对 ROS apt 源 / `/opt/ros/humble` / 底包三项回到 `MISSING`，`--strict` 退出 1。与干净机器的差异只在设计上保留的部分：rosdep / vcstool / colcon 工具、`rosdep init` 与缓存，以及 rosdep 装的系统依赖（OpenCV、cmake 等约 260 个非 ROS 包，apt 不视为自动安装，`autoremove` 不会回收）。

### `rospm build`
- [x] 封装 colcon，按叠加顺序 source underlay，注入清单中的 colcon 配置；`--dry-run`。
- [x] 真实机器上验证 `[workspaces.<name>.colcon]` 各字段的实际效果（2026-09-26）：`build_args`（`--symlink-install`，21 包 2m40s）；`defaults`（`COLCON_DEFAULTS_FILE` 生效，`packages-select` 只构建 1 包、`cmake-args` 进入 CMakeCache）；`mixins`（`--mixin release` / `debug` 切换 `CMAKE_BUILD_TYPE`）。发现 `mixins` 依赖 `python3-colcon-mixin` 且需用户自行 `colcon mixin add/update`，否则 colcon 报 `unrecognized arguments: --mixin`；`check` 现在在清单声明 mixins 时探测 `colcon mixin` 可用性。

### `rospm env` / `rospm shell`
- [x] `env --shell bash|zsh`；按 workspace 顺序 source，导出 `[env]`。
- [ ] `env --shell fish`（spec §4 已列出）。
- [x] `shell`：写入 `.rospm/env.sh` 与 `shellrc`，启动带前缀提示符的交互 bash。

### `rospm doctor`
- [x] 检测 git / vcs / colcon / rosdep / apt-get / dpkg-query / pixi / docker 与 `/opt/ros/*`，`--json`。
- [x] 输出 ROS apt 源、`universe`、locale 状态（2026-09-26）。

### 验收
- [x] 在一台干净 22.04 虚拟机上，仅凭清单 + 源码完成 `apply && build && shell`，`ros2 run` 成功（`apply` 须从无 ROS apt 源起步）。（2026-09-26，WSL2 Ubuntu 22.04.5，`examples/minimal-humble`，`ros2 run demo_nodes_cpp talker` 在 `rospm shell` 内正常发布）
- [x] 同样流程在 24.04 + Jazzy 上通过（2026-10-07，Ubuntu 24.04.5 x86_64 虚拟机；`apply`、`check --strict`、bash `env`、`build`、`ros2 run demo_nodes_cpp talker`、`lock`、`verify`、`deps check`、`uninstall --yes` 闭环；详见 [p1-native-validation.md](p1-native-validation.md)）。
- [x] `rospm uninstall --yes` 后 `rospm check` 对 ROS 各项回到 `MISSING`。（2026-09-26，三次 uninstall 均通过；第二轮 apply 的 `rosdep update` 因主机到 GitHub 的连接超时未完成，bootstrap 与密钥校验步骤已有实机证据）

---

## Phase 1.5 — 输出本地化 i18n（约 1 周）

状态：**完成**（2026-09-26）。决策 #16，规则见 spec §4.2。插在 P1 与 P2 之间：P1 的命令集已稳定，现存面向用户的字符串约 150 条（CLI 输出 43、计划注释与探测报告 68、`init --from-env` 说明 32），回填成本可控；P2 起每个阶段都会新增大量字符串，晚做只会更贵。不阻塞 P1 的干净虚拟机验收，可并行。

- [x] 选库并落基础设施（2026-09-26）：`rust-i18n`，资源 `crates/rospm-cli/locales/{en,zh-CN}.yml` 编译期嵌入。`rospm-core` 新增 `message::Message`（原样文本或键 + 参数）与 `msg!` 宏；计划标题 / 注释、探测报告各列、`Error`、`init --from-env` 的 notes / todos / 草稿注释全部改为 `Message`，CLI 用 `set_renderer` 安装渲染函数，核心库不依赖 locale（未安装渲染器时显示 `key(arg=value)`）。
- [x] locale 选择（2026-09-26）：`--lang` > `ROSPM_LANG` > `LANGUAGE`（冒号列表，取首个 `zh*` / `en*`）> `LC_ALL` > `LC_MESSAGES` > `LANG`；`zh*` → `zh-CN`，其余（含未设置 / `C` / `POSIX`）→ `en`。在构建 clap 命令之前完成，`--help` 与自实现的 `help` 子命令、章节标题均本地化。单测覆盖 `zh_CN.UTF-8` / `zh_TW` / `C` / `POSIX` / 未设置 / `LANGUAGE=zh:en` / `LANGUAGE=en:zh` / `LC_ALL` 优先级 / `--lang` 与 `ROSPM_LANG` 覆盖。
- [x] 回填现有字符串（2026-09-26）：clap 说明与 `-h` / `-V` 说明、`init`（`detected` / `TODO` 行、模板与草稿注释）、`check` 表格（状态列、主题列、期望 / 实际）与总结、`doctor`、`apply` / `uninstall` / `build` 计划标题与注释、确认提示、清单校验与 `Error` 全部变体。`--json` 键名与枚举值保持英文，自由文本随语言。已知限制：clap 自身的解析错误信息与 `[OPTIONS]` / `<COMMAND>` 占位符仍为英文。
- [x] `init` 模板按语言选文件（`templates/rospm.{en,zh-CN}.toml`），`--base` 改为按行重写 `ros_base`；单测断言两份模板（含 `--base` 改写后）都通过 `Manifest::parse`。
- [x] 单测：两种语言键集合与占位符集合一致（`_test.*` 键只在 en 中，用于测缺键回退）；缺键回退英文、两边都缺时返回键名，不 panic。CLI 集成测试显式设 `LANG=C` / `LANG=zh_CN.UTF-8`，断言中文输出与退出码不变、`--json` 枚举值仍为英文。CI 冒烟增加 `LANG=zh_CN.UTF-8` 一轮（`--help` / `check` / `apply --dry-run` / 错误）。
- [x] `docs/spec.md` §4.2 补充实现说明，README 增加 "Language / 语言切换"。
- [x] 与 P1 实机验证 / 镜像功能合并（2026-09-26）：密钥下载步骤、镜像注释、rosdep 源改写提示、`colcon mixin` 探测、测速表与 `--mirror` 全部提示走键；`Message` 不接受裸 `&str`，漏翻直接编译失败；`--mirror` 帮助隐藏 clap 附加的英文 possible-values。
- [x] **locale 探测与系统 locale 修改脱钩**（2026-09-26）：此前 `apply` 用当前进程的 `LC_ALL` / `LC_CTYPE` / `LANG` 判断"locale 是否 UTF-8"，用户以 `LANG=C` 调用（CI、脚本、或像 P1.5 测试那样临时切英文输出）就会触发系统级 `update-locale LC_ALL=en_US.UTF-8 LANG=en_US.UTF-8`，覆盖用户原有的 `/etc/default/locale`（2026-09-26 在验证主机上实际发生）。现在 `probe::LocaleState` 先读 `/etc/default/locale`（`KEY=value`，支持引号与注释，`LC_ALL` > `LC_CTYPE` > `LANG` 首个非空值），无该文件才退回进程环境；只在系统 locale 非 UTF-8 时才计划 `locale-gen` + `update-locale LANG=en_US.UTF-8`，不再设 `LC_ALL`，文件里的非 UTF-8 值来自 `LC_ALL` / `LC_CTYPE` 时以裸变量名移除；计划注释写明改的是 `/etc/default/locale` 与原值，shell 非 UTF-8 而系统已是 UTF-8 时只加一条注释不动系统。`check` / `doctor` 分 `system locale` / `shell locale` 两行（后者非 UTF-8 为 `info`，`--strict` 不受影响），`doctor --json` 的 `locale_utf8` 改为 `locale { system_file, system, system_utf8, shell, shell_utf8 }`。验证：单测覆盖文件解析与四种组合；在 `/etc/default/locale` 为 `zh_CN.UTF-8` 的主机上 `LANG=C rospm apply --dry-run` 不再含 locale 步骤；用 `unshare -rm` 绑定挂载伪造"无文件"与"`LC_ALL=C` 覆盖 `LANG`"两种文件状态，计划分别为 `update-locale LANG=en_US.UTF-8` 与 `… LANG=en_US.UTF-8 LC_ALL`；后者的移除语义用 `update-locale --locale-file=<临时副本>` 实测（`LC_ALL=C` 被注释掉，原有空 `LC_ALL=` 行保持不动）。输出语言选择与此无关，保持只看进程环境。
- **产出**：`LANG=zh_CN.UTF-8` 下 `--help` / `check` / `apply --dry-run` / 错误信息全部为中文，`LANG=C` 下全部为英文。此后 review 把裸英文用户字符串视为缺陷。

---

## Phase 2 — 锁文件与依赖补全（约 3 周）

状态：**单机实测通过，已知缺陷与设计问题已修复**（2026-09-27）。`lock` / `verify` / `deps` 与 `apply --locked` 都已在本机真实执行，两轮 `apply` → `uninstall` 闭环回到基线；过程中修复 3 个缺陷；2026-09-27 又修复坏源阻塞重跑、测试依赖主机 keyring、`verify` 漏报缺底包，并解决锁定版本不在快照中、顶层 Ubuntu 包精确固定两个设计问题。未完成：间隔数周复测、第二台机器（24.04）、干净虚拟机上用新规则真实 `apply --locked`、CI 场景。见 [p2-locked-validation.md](p2-locked-validation.md)。
前置：P0 的"ROS 快照签名密钥"核验必须先完成，否则 `apply --locked` 无法可信地切换源；P1.5 的 i18n 层必须先落地，本阶段新增字符串直接走该层。（两项均已满足）

### `rospm lock`
- [x] lock v0 schema 与解析（`Lock::parse`）。
- [x] 源码层：`vcs export --exact` → commit SHA（2026-09-26）。按叠加顺序对每个 workspace 导出一次，与 `.repos` 声明逐一对应；`url` 取 `.repos` 写法，新增可选字段 `version` 记录请求的分支 / 标签；vcs 缺失或导出失败退回 `git rev-parse HEAD`；声明了但未导入的仓库直接报错（锁描述的是已解析状态）。
- [x] apt 层：按平台记录已安装版本 + ROS / Ubuntu 快照日期（2026-09-26，spec §3.3）。记录全部已装 `ros-<distro>-*` + 底包 + 平台覆盖后的额外包 + 三个工具包，`.deb` sha256 尽力取自 `apt-cache show`；快照日期**发现**自 `snapshots.ros.org/<distro>/` 索引（最新三个日期依次试），核对 `Release` 的 `Suite` / `Architectures`，用 keyserver 取得并按指纹核验的密钥 `gpgv` 验签 `Release.gpg`；Ubuntu 快照时间戳由 ROS `Release` 的 `Date:` 换算，`InRelease` 须 200 并用 ubuntu-archive-keyring 验签。任一失败留空并输出 DEGRADED 说明；`ROSPM_NO_NETWORK=1` 跳过。主机 Ubuntu 与清单不符、`--platform` 非本机、dpkg 不可用均报错。本机实测：humble/jammy 发现 `2026-08-07`（Ubuntu `20260808T004205Z`），jazzy/noble 发现 `2026-09-11`（Ubuntu `20260911T182146Z`），两者签名均验证通过。
- [x] pip 层：`pip list --format=freeze` 结果只记录清单点名的包供 `verify` 对比（决策 #6）；pip 不可用只说明。
- [x] 记录清单哈希；同一输入产生同一输出（幂等）：内容不变则不重写、`generated_at` 保持并输出 `unchanged`；已有锁文件中其它平台的 apt 记录原样保留。
- [x] 锁文件读写往返测试（`tests/lock.rs`：合成主机 + 真实本地 git 仓库 + 罐装快照服务器；CLI 测试用 `ROSPM_NO_NETWORK=1`；`#[ignore]` 的真实网络测试本机通过）。

### `rospm verify`
- [x] 对比锁文件与当前环境（2026-09-26，spec §3.4）：清单哈希与提供者、Ubuntu 版本、每条 git 源码的 HEAD 与 origin、本机平台的 apt 版本（其它平台合并为一行 info）、pip 版本；快照日期作 info 展示；一致退出 0，否则 1；清单哈希不一致提示重新 `lock`。本机实测：`lock` 后 `verify` 全部 ok；上游多提交一次并 pull 后 `verify` 对该仓库报 `MISMATCH`（给出两个短 SHA）退出 1。

### `rospm apply --locked`
- [x] 把 sources.list 切到 `snapshots.ros.org` 与 `snapshot.ubuntu.com`（2026-09-26，spec §3.2）：复用 P1 的 `ros2.list` 路径，`signed-by=` 换成从 keyserver 按指纹核验的快照 keyring；Ubuntu 快照写 rospm 自己的 `rospm-ubuntu-snapshot.list`（直接 URL 形式，不需要 `--snapshot`），不改 Ubuntu 自身的源。按锁定版本 `apt-get install --allow-downgrades <名>=<版本>` 安装，`git checkout --detach <rev>` 固定源码，跳过 `rosdep install`。锁文件缺失 / 与清单不一致 / 无本机平台记录 / 无底包记录都报错。
- [x] 覆盖 updates / security 组件与 arm64（2026-09-26）：Ubuntu 快照文件含 `<codename>` / `-updates` / `-security` × 四个组件；`lock` 核对快照 `Release` 的 `Architectures` 含本机架构，实测 humble `2026-08-07` 与 jazzy `2026-09-11` 均列出 `amd64 arm64 armhf i386`。arm64 主机上的真实 `apply --locked` 未做。
- [x] 快照不可用时按 spec §3.2 降级并明确告警（2026-09-26）：锁文件中快照为空或真实运行前 `InRelease` 探测不通时退回实时源，打印 `WARN: apt snapshot unreachable, falling back to version check`，版本仍按锁文件安装。真实运行的探测路径只有代码审查与 dry-run 覆盖（本机不做真实 `apply`）。
- [x] 真实执行 `apply --locked` 一次（快照 apt 索引可用性、`--allow-downgrades` 与依赖解析是否成立、`rosdep update` 之后 `deps check` 是否一致）。
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

### `rospm deps check` / `rospm deps install`
- [x] 解析 workspace 中全部 `package.xml`，与已安装集合求差（2026-09-26，spec §4 表）。依赖键分内部 / 外部 / 跳过；外部键经 `rosdep check` + `rosdep resolve` 映射到 apt 包后查 dpkg；rosdep 输出解析为纯函数并用罐装文本单测；`package.xml` 解析失败也在表中报出。本机实测（rosdep 已 update）：`rclcpp` → `ros-humble-rclcpp` 未安装 `MISSING`，`libopencv-dev` 已装给出版本。
- [x] `install` 交给 rosdep 执行，支持 `--dry-run`（2026-09-26）：命令形状与 `apply` 第 4 步一致，不做 `rosdep init`（提示先 `rospm apply`）、不测速；rosdep 禁用时只有一条说明。

### 验收
- [ ] 两台机器、间隔数周执行真实 `apply --locked`，`verify` 报告一致（本机时间点 2 已完成，第二台机器与后续间隔复测仍待执行）。
- [ ] CI 增加 `lock` → 新容器 `apply --locked` → `verify` 场景。

---

## Phase 3 — aarch64 与平台覆盖（约 3 周）

状态：**未开始**。清单的 `[target.<platform>]` 覆盖/替换/排除已在 schema 与 `check --target` 中实现，但仓库索引查询与映射表未做。

- [x] 清单 `[target.<arch>]` 段：覆盖、替换、排除（schema + `effective_apt` 合并）。
- [ ] `rospm check --target aarch64`：查询 apt / conda 仓库索引，报告目标架构缺失的包。
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
