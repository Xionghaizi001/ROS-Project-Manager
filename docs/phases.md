# rospm 分阶段实施清单

> 记录当前及后续阶段任务，状态更新日期：2026-10-21。
> 总览见 [ROADMAP.md](../ROADMAP.md)；决策见 [decisions.md](decisions.md)；格式与命令语义见 [spec.md](spec.md)。

图例：`[x]` 完成 · `[~]` 部分完成 · `[ ]` 未开始。
每个阶段末尾的“产出”是进入下一阶段的门槛。

## 长期测试记录与早期阶段剩余验收

已清理 Phase 0–2 的阶段实施说明，长期实测证据继续保留在 [P0 外部核验](p0-external-validation.md)、[P1 native 验收](p1-native-validation.md)、[P2 锁定重放](p2-locked-validation.md)。以下时间点记录原样保留，供后续跨机器、间隔复测比较。

- [x] `apply --locked` 的长期实测与相关修复：
  - [x] 时间点 1（2026-09-26，22.04 / Humble）非破坏性核验，脚本 `scripts/p2-locked-sandbox-check.sh`：真实 `lock` 发现并验签 `2026-08-07` / `20260808T004205Z`；隔离 apt 目录中两个快照的索引可下载；按快照合成的锁在空 dpkg 状态下完整解析 645 个包，`--allow-downgrades` 对 `python3-rosdep` 生效。
  - [x] 修复：写 `rospm-ubuntu-snapshot.list` 后缺少 `apt-get update`，锁定安装原本看不到 Ubuntu 快照索引（`tests/plan.rs` 加断言）。
  - [x] 真实闭环：实时 `apply` → `build` / 运行节点 → `lock` → `verify` → `uninstall`，已装包列表与基线完全一致；快照锁真实 `apply --locked` 成功（212 个包、`python3-rosdep` 降级、`git checkout --detach`），`verify` / `deps check` / `check --strict` 全部就绪，节点用锁定的 rclcpp 16.0.19 编译运行，重新 `lock` 与输入逐条一致，`uninstall` 后只剩按设计保留的工具版本差异。
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
- [x] `apply` 可安装指定的 ROS 发行版，而不是默认的 Jazzy。已实现：`apply --distro` 持久化选择，`init` 支持指定发行版及按主机选择默认。
  - [x] `apply --distro <humble|jazzy|rolling>`：与清单不同时，先核对主机 Ubuntu（Humble → 22.04，Jazzy / Rolling → 24.04），不符直接报错、不改任何东西；相符则把 `ros_distro` 写回清单（`ros_base` 若为 `ros-<旧发行版>-*` 一并改名），显式的 `environment.native.ubuntu` 同步为目标版本，再按新发行版执行；保留清单注释，`--dry-run` 只预览而不写回。发行版随项目走：写回清单而非一次性覆盖，保证随后的 `lock` / `verify` / `build` / `env` / `uninstall` 看到同一发行版。
  - [x] `--locked` 与 `--distro` 不同时，锁文件哈希必然不匹配，直接报错提示先 `apply --distro` 再 `lock`。
  - [x] 本机已装另一发行版时只在计划中说明（不自动卸载旧发行版，`uninstall` 按清单只卸当前发行版）。
  - [x] `init --distro <名>`；不带参数时按主机 Ubuntu 选默认（22.04 → Humble，24.04 → Jazzy），无法识别时仍为 Jazzy。
  - [x] 测试：计划输出（源 codename、底包名）、清单写回、Ubuntu 不符报错、`--locked` 冲突；`spec.md` 命令表同步。
- [ ] 两台机器、间隔数周执行真实 `apply --locked`，`verify` 报告一致（22.04 / Humble 与 24.04 / Jazzy 的已有记录见上；独立机器同锁比较与后续间隔复测仍待执行）。
- [ ] CI 增加 `lock` → 新容器 `apply --locked` → `verify` 场景。

---

## Phase 3 — aarch64 与平台覆盖（约 3 周）

状态：**QEMU 开发验收完成，原生 ARM 发布验收待执行**。目标仓库索引查询已完成并通过回归验证；内置替换映射表已完成；已搭建 Docker + QEMU 环境并通过 aarch64 集成验收，正式发布前再使用原生 ARM runner 复测。

- [ ] 正式发布前使用 GitHub `ubuntu-24.04-arm` 原生 runner 复测同一 fixture 的锁定还原、编译和节点运行，保留日志（发布门槛，当前不依赖 runner）。
- **产出**：开发阶段已验证 x86 最终生成的多平台锁在 QEMU aarch64 上 `apply --locked`、构建和节点运行成功，缺失包有明确替代或明确报错。正式发布前通过原生 ARM runner 复测；当前未验证 Raspberry Pi / Jetson 的硬件接口与驱动。

---

## Phase 3.5 — Conda 自动配置与一键还原（约 2–3 周）

状态：**进行中**（2026-10-20，已完成项目级 Conda 配置、CLI 覆盖、已有 / 受管环境区分、Conda 自动引导及受管环境的本机平台锁定重放；未安装 Conda 时 `apply` 可自动安装 Miniforge 并创建受管环境，`lock` / `apply --locked` 按显式锁精确重放）。排在 Phase 3 之后、Phase 4 的 schema v1 冻结之前；支持使用已有 Conda 环境，也支持在用户未安装 Conda 时自动安装、创建项目环境并继续还原，不依赖 Phase 8 的 Pixi 提供者或 GUI。

需求与合理性：项目可能依赖只安装在特定 Conda 环境中的 Python 包或工具，显式选择环境能避免依赖用户当前终端的激活状态，并让本地与 CI 的构建入口一致。但仅激活环境不足以保证成功：colcon 自身的解释器、CMake 缓存的 Python 路径，以及 ROS Python 扩展的 ABI 都可能仍指向系统环境。尤其是 apt 安装的 ROS 与 Conda Python 不一定兼容，因此需要在编译前检查并明确报告冲突，不能承诺任意环境均可编译。

一键还原要求：当清单声明使用 rospm 管理的 Conda 环境时，缺少 Conda 应由还原流程自动补齐，无需用户另行安装、手动激活或重新执行命令。rospm 负责编排工具安装、环境创建、依赖安装与后续还原，依赖求解和包安装交给 Conda；主机仍须满足清单声明的操作系统、架构及驱动等前置条件。

- [x] 定义可选的项目级 Conda 构建配置及 CLI 覆盖：native 清单 `[build.conda]` 必须且只能设置 `name` / `prefix`，`build --conda-env <name>` / `--conda-prefix <path>` 互斥并覆盖整个默认选择，不写回清单；清单相对路径以清单目录为基准，CLI 相对路径以当前目录为基准。配置后通过 `conda run` 构建，未配置时保持原构建方式；已同步 `spec.md`、Rust manifest schema、`examples/conda`、双语模板与中英文帮助。回归覆盖配置校验与序列化、覆盖优先级、相对 / 绝对 / 含空格路径、拓扑与 colcon 参数保留、预览无写入、Conda 子进程调用及失败状态；环境准备与兼容性检查仍由后续项完成。
- [x] 区分“使用已有环境”与“由 rospm 管理环境”：新增 `[build.conda.managed]`，与已有环境的 `name` / `prefix` 互斥，必填 Python 版本与 channels，可选 `environment_file` / `lock_file` 引用，相对路径以清单目录为基准。受管 prefix 固定为项目的 `.rospm/conda/env`；`apply` 使用已有 Conda 创建环境后继续原还原流程，`build` 只检查和使用已准备环境；已有环境缺失时明确报错且不修改外部环境。创建成功才写定义摘要完成记录，匹配时复用，不接管已有目录或未完成环境。已同步 schema、规格、双语模板与示例；测试覆盖互斥与序列化、路径解析、项目隔离、创建 / 复用 / 失败、只读检查、预览无写入及中英文错误。Conda 自动安装由下一项实现；精确重放由第 4 项实现；自动恢复与更新由幂等性任务完成。
- [x] Conda 自动引导：探测并复用兼容的已有 Conda；没有时按主机操作系统 / 架构选择固定版本的 Miniforge 安装器，从官方来源下载并校验 SHA256，非交互安装到 rospm 管理的用户目录。该步骤无需 sudo，不修改 `.bashrc` 等 shell 启动文件、不执行全局 `conda init`；安装完成后在同一次还原中继续执行。安装器版本、来源与摘要应由受信任的配置固定，摘要不符时拒绝执行。
  - [x] 内置表 `crates/rospm-core/data/miniforge.toml` 固定 Miniforge `26.7.2-0`（conda-forge GitHub Release），x86_64 / aarch64 的 SHA256 经本机下载 `sha256sum` 与官方 `.sha256` 双重核对；最低兼容 conda 24.3。受管模式的 Conda 选择顺序：rospm 已就绪的 Miniforge → `PATH` 中版本合格的 `conda` → `apply` 自动安装（`build` 只报错并提示 `apply`）；已有环境模式不自动安装。规划阶段只读文件，不启动子进程、不下载。
  - [x] 安装器缓存于 `~/.cache/rospm/downloads`，`.part` 校验通过才改名，摘要不符删除并拒绝；摘要一致的缓存离线复用，损坏缓存重新下载；下载有超时与重试。`bash --noprofile --norc <安装器> -b -p ~/.local/share/rospm/conda/miniforge3-<版本>-<subdir>`，`conda --version` 合格后才写 `.ready`；无记录的安装目录视为中断，重跑时删除重装。`--dry-run` 展示选择顺序、版本、URL、摘要、缓存与安装位置。中英文消息、`spec.md` 同步。
  - [x] 测试：单元测试覆盖内置表、版本比较、缺失 / 版本过低时引导、引导后 `apply` / `build` 使用安装的 conda、摘要不符拒绝且不留文件、损坏缓存替换、离线无缓存报错、安装中断后离线从缓存重装；CLI 测试覆盖预览不写文件及 `build` 中英文提示。真实验收（2026-10-20，Docker `ubuntu:22.04`、uid 1000、无 Conda）：`tests/conda_bootstrap.rs`（`--ignored`）真实下载 / 缓存校验、批处理安装、以安装的 conda 创建含 numpy 的环境，`conda run` 使用环境内解释器，删除就绪记录后从缓存重装，`.bashrc` 未变（Conda 自身仅新增 `~/.conda`、`~/.cache`）。本机网络对 conda.anaconda.org 多次断流，环境创建经清华 conda-forge 镜像完成。aarch64 安装器的真实执行留给干净机一键验收。
- [x] 环境创建与依赖安装：调用 Conda 按项目定义创建环境并安装构建依赖；先支持本机平台的精确锁定重放，定义锁文件引用、摘要及 Conda / pip 依赖覆盖规则。`apply --locked` 必须使用完整的目标平台锁记录，缺失时明确失败，不重新求解；外部已有环境只做检查，不自动获得可重放保证。后续 Phase 8 继续提供基于 `pixi.lock` 的多平台环境管理。
  - [x] 锁格式：每平台一份 Conda 显式锁（`conda list --explicit --sha256`，每包带摘要），路径为 `lock_file`（`{platform}` 展开为 `linux-64` / `linux-aarch64`，默认 `conda-{platform}.lock`；多平台项目的显式路径必须含占位符）；`rospm.lock` 新增 `[[conda]]`，只记平台、锁文件路径、锁文件 SHA256 与规范化定义 SHA256，无 Conda 时序列化不变。普通 `apply` 仍按定义求解，`lock_file` 不再导致拒绝。
  - [x] `rospm lock`：受管环境就绪且定义一致时写显式锁（内容不变不重写），校验平台与摘要，并把锁摘要补入完成记录，使本机 `apply --locked` 直接复用；其它平台的 Conda 记录保留；未就绪、Conda 查询失败、外部环境均只说明不记录。JSON 报告新增 `conda`。
  - [x] 覆盖规则：显式锁只含 Conda 包；定义中的 pip 依赖须全部为 `name==version`（不接受范围、URL、extras、标记），重放以 `--no-deps` 安装，`lock` 核对环境内全部 PyPI 包与固定完全一致；`variables` 重放后以 `conda env config vars set` 写入。
  - [x] `apply --locked`：规划阶段（无写入、无子进程）要求本平台记录存在、定义摘要一致、锁文件存在且摘要一致、平台 / `@EXPLICIT` / 每包摘要有效、覆盖定义中的 Conda 依赖与 Python 版本（有 pip 依赖时含 `pip`）、pip 全部固定，否则失败并提示在该平台 `apply` 后重新 `lock`，从不求解。执行 `conda create --yes --prefix … --file <锁>` → 变量 → `pip install --no-deps`，全部成功才写两行完成记录；普通 `apply` / `build` 复用重放的环境，按定义求解的环境不被视为锁的重放（不覆盖目录）。外部环境在 `--locked` 下只检查并在计划中说明无可重放保证。
  - [x] 测试：单元测试覆盖记录 / 幂等 / 同机复用 / 干净项目重放不求解 / 两行完成记录、缺平台记录、定义过期、摘要不符、平台不符、缺 `@EXPLICIT`、缺包摘要、覆盖不全、Python 版本不符、pip 未固定、PyPI 漂移、外部环境、`{platform}` 展开与多平台校验、锁合并保留其它平台；CLI 测试覆盖 `apply --locked --dry-run` 缺记录的中英文错误与按锁重放预览（无写入），以及普通 `apply` 不受 `lock_file` 影响。真实验收（2026-10-20，本机 x86_64、conda 26.7.1，清华 conda-forge / PyPI 镜像）：`tests/conda_bootstrap.rs` 的 `locks_and_replays_a_real_environment`（`--ignored`）按定义求解含 `six==1.16.0` 与变量的环境 → `resolve_lock` 写锁 → 同机 `--locked` 复用 → 干净项目按锁重放，`conda list --explicit --sha256` 与锁逐字一致、`six` 与变量生效，重放环境再次锁定得到相同记录（约 150 秒）。aarch64 的真实重放留给干净机一键验收；`verify` 对 Conda 锁的核对见下方“对接 `env` / `shell`”一项。
- [x] 定义一键还原入口（`apply --build`，与 `--locked` 组合）：自动准备 Conda 工具与项目环境，继续完成系统依赖安装、源码还原、按拓扑顺序编译及清单声明的验证命令。普通 `apply` 仍负责环境与源码准备；`build` 使用已准备好的环境。用户无需手动激活环境或在安装 Conda 后重新发起还原。
  - [x] 已实现（2026-10-21）：`apply --build` 在 apply 计划成功后重新探测主机，再生成与 `build` 相同的全量编译计划，并追加清单新增的 `build.verify` 命令（Bash，加载与 `rospm env` 相同的环境，声明 Conda 时经 `conda run` 执行）；`build` 不执行验证命令；apply 失败时不编译；`--dry-run` 依次预览 apply 与编译 / 验证计划且不写文件。已同步 schema、`spec.md`、双语模板与帮助。测试：`tests/plan.rs` 覆盖验证命令顺序、环境、Conda 包装与空命令校验；CLI 测试覆盖 `apply --build --dry-run` 的预览顺序、无写入及 `build` 不执行验证。真实主机上的一键执行由下方“干净机一键验收”完成。
- [ ] 安装与还原步骤幂等：复用已校验的工具、下载缓存和符合定义 / 锁记录的项目环境，失败后重跑可继续；未完成的工具或环境不得标记为就绪，不修改外部已有环境。`--dry-run` 展示安装器版本、工具与环境位置、依赖输入及后续命令，不下载或写文件；下载、校验、安装失败时报告具体步骤，离线且无所需缓存时明确报错。
  - [x] 已实现（2026-10-22）：受管环境创建前写入 `pending` 完成记录标明 prefix 归 rospm 所有，成功后才改为定义摘要；重跑 `apply` 时，有记录但不匹配的 prefix（创建或 pip 步骤中断、定义变化、`--locked` 遇到按定义求解的环境）删除后重建，无记录的目录仍视为外部目录拒绝覆盖。`--dry-run` 依次说明复用 / 重建 / 冲突，就绪时只显示复用、不再列出创建命令。Miniforge 侧的已校验缓存复用、中断重装、摘要不符拒绝、离线无缓存报错与按步骤报错沿用引导项的实现。测试覆盖失败后重跑重建并清除残留文件、定义变化重建、`--locked` 替换求解环境、外部目录不被触碰，以及预览中的复用 / 重建 / 冲突说明。
- [ ] 使用 `conda run -n <name>` / `-p <path>` 或等价的受控子进程执行构建，不依赖调用终端已激活环境；验证环境存在，按 workspace 拓扑顺序加载兼容的 ROS / underlay，并让 `--dry-run` 展示环境、解释器与完整构建命令。直接 `build` 时缺少 Conda、环境或构建工具应明确报错并提示先执行 `apply`，不静默回退到系统环境。
- [ ] 确保 colcon、Python 构建步骤与 CMake 的 `Python3_EXECUTABLE` / `PYTHON_EXECUTABLE`（按包所用查找机制）使用所选环境的解释器；检查所需 Python 模块及 ROS Python 扩展能否导入，发现版本 / ABI 不兼容时给出诊断。切换环境时检测旧构建目录中的解释器缓存，提示清理或隔离构建目录。
- [ ] 对接 `env` / `shell` 的 Conda 环境选择，验证安装生成的 Python 入口及 `ros2 run` 使用目标解释器，必要时可清除节点文件的 `shebang` ；native 的 apt 锁记录与受管 Conda 环境锁记录分别校验，明确记录环境名称或路径本身不等于锁定包版本。
- [ ] 验收：未激活 Conda 的终端中，指定名称和路径均能完成最小 ROS Python 包构建并运行节点，能导入仅目标环境安装的依赖；覆盖构建预览不写文件、缺环境与 ABI 冲突诊断、切换环境后的缓存检测；未配置 Conda 的 native 构建回归通过。
- [ ] 干净机一键验收：在满足 ROS 主机前置条件、未安装 Conda 的机器上，仅凭项目清单、目标平台锁文件和源码入口，一条命令完成 Conda 安装、项目环境创建、依赖与源码还原、编译和最小节点运行验证。覆盖 x86_64 / aarch64 安装器选择、重复执行复用、安装中断后重试、摘要错误、缺少锁记录，以及 shell 启动文件保持不变；锁定重放在另一干净环境中得到相同包记录。
- **产出**：已有 Conda 用户可直接指定环境；未安装 Conda 的用户可一条命令自动准备工具和项目环境并完成还原、编译与验证；不兼容环境在构建前得到明确诊断。

---

## Phase 3.6 — 常见 C++ / Python 错误诊断（约 1–2 周）

状态：**未开始**（2026-10-19 新增需求）。排在 Phase 3.5 之后、Phase 4 发布之前；首期服务 CLI 构建与一键还原，诊断结构供后续 GUI / 编辑器复用。

需求与合理性：项目管理器掌握构建命令、workspace、依赖清单与实际 Python 环境，可将编译器 / 解释器已有的错误信息整理为“文件与行列 + 问题说明 + 排查建议”，帮助用户从长日志中定位问题。当前执行层直接显示子进程输出，失败时仅记录命令与退出状态，尚需补充日志采集和诊断解析。首期使用可测试的规则与工具原生诊断，不依赖在线 AI；只能解释捕获到的报错，不承诺推断任意业务逻辑错误或准确找出所有根因。

- [ ] 构建日志采集：执行子进程时同时转发并保存 stdout / stderr，记录命令、工作目录、workspace / ROS 包、退出状态和所选 Python 环境；避免管道阻塞与无界内存缓存，兼容 ANSI 颜色及 colcon 多包并行输出，优先按包归属解析日志。首期覆盖 `build`、一键还原中的构建与非交互验证命令；用户在交互 shell 中自行启动的程序不自动获得诊断。
- [ ] 在 `rospm-core` 定义统一诊断结构：稳定错误码、类别、严重程度、工具、文件 / 行 / 列、原始报错片段、关联诊断、解释与建议，并区分“工具已明确报告的事实”与“待验证的可能原因”。位置缺失时不编造行号；显示编译器报告的位置，不把它当作根因必然所在的位置。
- [ ] C++ 首批规则：GCC / Clang 的语法与类型错误、缺少头文件；CMake 的 `find_package` 失败；链接器的 `cannot find -l...` / `undefined reference`。保留模板实例化和 include 链等关联上下文，区分语法、包发现、头文件搜索与链接问题；优先使用工具支持的结构化诊断，兼容常见文本输出与无法识别的格式。
- [ ] Python 首批规则：`SyntaxError`、`IndentationError` / `TabError`、`ModuleNotFoundError`、常见 `ImportError` 及 traceback 位置；保留异常链与项目调用帧，对运行时异常按实际异常类型解释。模块缺失时检查本次构建 / 验证所用解释器，避免把“装在另一个环境”误报为完全未安装。
- [ ] 依赖建议结合 `package.xml`、rosdep、项目清单及当前提供者：有明确映射和检查证据时提示缺少的依赖及对应还原 / 安装步骤；无法确认时提示“未找到头文件 / 模块，可能缺少依赖或搜索路径未配置”，不把 Python 模块名直接当作 pip / Conda 包名，不把所有链接错误归为缺包。诊断提供排查建议，不自动修改源码或安装依赖。
- [ ] CLI 在失败后输出简短摘要，按包归组、合并重复诊断并优先展示直接报错，将 colcon / make 的失败汇总关联为后续结果；不将日志中的第一个错误武断视为唯一根因。示例：`src/main.cpp:42:10：编译器报告此处缺少分号；请检查此处及前一条语句。`、`节点使用的 Python 环境中未找到模块 numpy；请核对项目依赖和环境配置。` 保留完整日志路径，解释与建议走中英文 i18n；`--json` 输出稳定字段，原始子进程输出不混入 JSON 数据流。
- [ ] 未识别错误时保留原始报错并提示查看日志；解析失败不得覆盖原命令失败状态，不得将失败改为成功。同步命令帮助、日志位置、诊断覆盖范围和 JSON 约定。
- [ ] 验收：用 GCC / Clang、CMake / 链接器及 Python 的真实失败样例验证位置、类别和建议；覆盖多包并行、重复与级联错误、模板上下文、异常链、路径含空格、ANSI 颜色、缺失位置、未知格式与成功日志不误报。增加最小 C++ / Python 失败项目的 CLI 集成验收，验证实时输出、完整日志、中英文摘要、JSON 可解析及失败退出状态。
- **产出**：常见 C++ / Python 报错可定位、可解释并提供有依据的排查建议；未知错误仍可追溯完整日志，CLI / GUI 共用诊断结果。

---

## Phase 4 — Release 与 schema v1（约 1–2 周）

状态：**未开始**。

- [ ] 完成 aarch64 与 Phase 3.5 Conda 自动配置、一键还原验收后的回归测试、文档与示例清单整理。
- [ ] 完成 Phase 3.6 常见错误诊断验收，确认原始日志、失败状态与机器可读输出不受影响。
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
- [ ] shell 补全、`doctor` 扩展自检、基于 Phase 3.6 扩展诊断规则与提示、文档站。

---

## Phase 7 — GUI（选装，独立排期）

状态：**未开始**。

- [ ] Tauri 或 egui，只调用与 CLI 相同的库层（`rospm-core`），不另写业务逻辑。
- [ ] 功能：清单编辑、check / verify 结果可视化、导出向导；复用 Phase 3.6 的结构化诊断，展示错误位置、解释、建议与原始日志。

---

## Phase 8 — Pixi 提供者（约 3 周）

状态：**进行中**（2026-09-26）。`ProviderKind::Pixi` 仍为 `Unimplemented` 占位；`pixi.toml` 生成的核心库（`rospm-core::pixi`）已合并，尚未接入 CLI。
前置：Phase 3 的 aarch64 验收、Phase 4 的 1.0 release 与 Phase 7 的 GUI；P0 的"补丁版 rosdep ROS 包映射"与"真实 ARM pixi-pack"两项核验。

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

状态：**未开始**。`ProviderKind::Docker` 目前为 `Unimplemented` 占位，前置为 Phase 3 的 aarch64 验收、Phase 4 的 1.0 release 与 Phase 7 的 GUI。

- [ ] 从清单生成 `Dockerfile`（分层：底包 → rosdep 依赖 → 源码 → 编译）、`compose.yaml`、`devcontainer.json`。
- [ ] 锁定记录 image digest；`apply` 即 build，`shell` 即 `exec`。
- [ ] 用 `buildx` 支持 `--platform linux/arm64`，复用 Phase 3 的 QEMU 测试环境验证跨架构镜像。
- **产出**：`rospm apply` 直接产出可运行的镜像，`rospm shell` 进入容器且 ROS 环境就位。
