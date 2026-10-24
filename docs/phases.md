# rospm 分阶段实施清单

> 记录当前及后续阶段任务，状态更新日期：2026-10-23。
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
- [ ] 两台机器、间隔数周执行真实 `apply --locked`，`verify` 报告一致（22.04 / Humble 与 24.04 / Jazzy 的已有记录见上；独立机器同锁比较与后续间隔复测仍待执行）。
- [ ] CI 增加 `lock` → 新容器 `apply --locked` → `verify` 场景。

---

## Phase 3 — aarch64 与平台覆盖（约 3 周）

状态：**QEMU 开发验收完成，原生 ARM 发布验收待执行**。目标仓库索引查询已完成并通过回归验证；内置替换映射表已完成；已搭建 Docker + QEMU 环境并通过 aarch64 集成验收，正式发布前再使用原生 ARM runner 复测。

- [ ] 正式发布前使用 GitHub `ubuntu-24.04-arm` 原生 runner 复测同一 fixture 的锁定还原、编译和节点运行，保留日志（发布门槛，当前不依赖 runner）。
- **产出**：开发阶段已验证 x86 最终生成的多平台锁在 QEMU aarch64 上 `apply --locked`、构建和节点运行成功，缺失包有明确替代或明确报错。正式发布前通过原生 ARM runner 复测；当前未验证 Raspberry Pi / Jetson 的硬件接口与驱动。

---

## Phase 3.5 — Conda 自动配置与一键还原

状态：**已有环境验收通过，干净机一键验收待执行**（2026-10-22）。支持使用已有 Conda 环境，也支持在用户未安装 Conda 时自动安装、创建项目环境并继续还原，不依赖 Phase 8 的 Pixi 提供者或 GUI。

一键还原要求：当清单声明使用 rospm 管理的 Conda 环境时，缺少 Conda 应由还原流程自动补齐，无需用户另行安装、手动激活或重新执行命令。rospm 负责编排工具安装、环境创建、依赖安装与后续还原，依赖求解和包安装交给 Conda；主机仍须满足清单声明的操作系统、架构及驱动等前置条件。

- [x] 验收：未激活 Conda 的终端中，指定名称和路径均能完成最小 ROS Python 包构建并运行节点，能导入仅目标环境安装的依赖；覆盖构建预览不写文件、缺环境与 ABI 冲突诊断、切换环境后的缓存检测；未配置 Conda 的 native 构建回归通过。
  - 2026-10-22：清理并重建 Ubuntu 22.04 / Humble 虚拟实例后，真实验收 13 项通过，CLI 构建回归 8/8 通过；记录、环境依赖与复跑脚本见 [P3.5 已有 Conda 环境验收](p35-existing-conda-validation.md)。
- [ ] 干净机一键验收：在满足 ROS 主机前置条件、未安装 Conda 的机器上，仅凭项目清单、目标平台锁文件和源码入口，一条命令完成 Conda 安装、项目环境创建、依赖与源码还原、编译和最小节点运行验证。覆盖 x86_64 / aarch64 安装器选择、重复执行复用、安装中断后重试、摘要错误、缺少锁记录，以及 shell 启动文件保持不变；锁定重放在另一干净环境中得到相同包记录。
- **产出**：已有 Conda 用户可直接指定环境；未安装 Conda 的用户可一条命令自动准备工具和项目环境并完成还原、编译与验证；不兼容环境在构建前得到明确诊断。

---

## Phase 3.6 — 常见 C++ / Python 错误诊断

状态：**进行中**（2026-10-23，已完成构建日志采集、统一诊断结构及 C++ / Python 首批规则）。排在 Phase 3.5 之后、Phase 4 发布之前；首期服务 CLI 构建与一键还原，诊断结构供后续 GUI / 编辑器复用。

需求与合理性：项目管理器掌握构建命令、workspace、依赖清单与实际 Python 环境，可将编译器 / 解释器已有的错误信息整理为“文件与行列 + 问题说明 + 排查建议”，帮助用户从长日志中定位问题。当前执行层直接显示子进程输出，失败时仅记录命令与退出状态，尚需补充日志采集和诊断解析。首期使用可测试的规则与工具原生诊断，不依赖在线 AI；只能解释捕获到的报错，不承诺推断任意业务逻辑错误或准确找出所有根因。

- [x] 构建日志采集：执行子进程时同时转发并保存 stdout / stderr，记录命令、工作目录、workspace / ROS 包、退出状态和所选 Python 环境；避免管道阻塞与无界内存缓存，兼容 ANSI 颜色及 colcon 多包并行输出，优先按包归属解析日志。首期覆盖 `build`、一键还原中的构建与非交互验证命令；用户在交互 shell 中自行启动的程序不自动获得诊断。
  - [x] 已实现（2026-10-22）：`Plan` 新增日志路径、`Command` 新增日志头；`build` / `apply --build` 的 colcon 与 `build.verify` 命令以两条读线程、8 KiB 固定缓冲转发 stdout / stderr 并原样（含 ANSI）写入 `.rospm/log/build.log`（每次覆盖，首个记录命令前提示路径），写入失败仍持续读取以免子进程阻塞，退出状态照常决定成败。日志头记录命令、工作目录、workspace 或验证命令、所选 Python 环境（`system` / `conda -n|-p …`）及 colcon 按包日志路径——包归属直接取 colcon 自身的 `log/latest_build/<包>/stdout_stderr.log`，不从并行交错输出中拆分（log base 依次取 `--log-base`、`$COLCON_LOG_PATH`、`log`）。子进程输出改经管道后，GCC 等工具检测到非终端可能不再输出颜色。Conda 检查与 shebang 改写不记录。测试：核心测试覆盖转发与保存、ANSI 原样、stderr、未标记命令不记录、失败后停止且保留退出状态、仅提示一次路径、含空格目录；计划测试覆盖 workspace / 验证命令的日志头与 Conda 环境；CLI 测试覆盖预览不建日志、真实失败构建写入日志头与退出状态。真实验收（2026-10-22，本机 Ubuntu 22.04 / Humble、GCC 11.4，项目路径含空格）：一个缺分号的 ament_cmake 包与一个 ament_python 包并行构建，合并日志完整保存日志头、并行交错的 colcon 输出、GCC `main.cpp:3:3: error` 及源码摘录、gmake 级联报错与 `exit status: 2`，记录的按包日志路径存在且含同一报错。同时发现：调用终端激活 Conda base 时，native 构建的 CMake 选中 Conda 的 `python3`，报 `ModuleNotFoundError: No module named 'catkin_pkg'`，已由下一项处理。
- [x] 构建环境基准（2026-10-22 新增并完成）：native 构建（未声明 `build.conda`）时调用终端激活的 Conda 环境不是目标环境，若其中缺少构建所需模块，则对构建命令取消激活后再编译。
  - [x] 已实现：`Host` 记录 `$CONDA_PREFIX`；有激活环境且未选择 Conda 时计划加一条说明，每条 colcon 与 `build.verify` 命令在加载 ROS / underlay 后检查 `python3` 能否导入 `catkin_pkg`、`em`、`rclpy._rclpy_pybind11`，能导入则保持环境，否则从 `PATH` 去掉该环境及其 Conda 根目录（含 `conda-meta` 才视为根，避免 `/usr/bin/conda` 误删 `/usr`）的条目、`unset CONDA_PREFIX` 等变量（CMake FindPython 同样读取 `CONDA_PREFIX`）并在 stderr 说明。旧 `build/*/CMakeCache.txt` 仍引用该环境时 CMake 会继续使用它，因此列出缓存并失败，提示删除对应 build / install 或改用 `--conda-env` / `--conda-prefix`，与 Conda 构建的旧缓存策略一致、不自动删除。只在执行时检查，`--dry-run` 不启动子进程。测试：单元测试真实执行该脚本（不可用环境被取消激活、可用环境保留、旧缓存报错并列出），计划测试覆盖仅在“有激活环境且未选择 Conda”时加入、位于 ROS 加载之后、覆盖验证命令。真实验收（本机 22.04 / Humble，激活 miniconda base）：此前失败的 C++ + Python 工作区构建成功且 CMake 使用 `/usr/bin/python3`；先用 Conda Python 配置过的工作区报出旧缓存，按提示删除后构建成功。
  - [x] 兜底（2026-10-22）：构建仍失败时（colcon 命令失败，或 Conda 构建的旧缓存检查失败），交互终端中列出所构建 workspace 现有的 `build/` / `install/` 并询问是否删除后重试，默认否；确认后删除（Rust `remove_dir_all` 不跟随符号链接，`--symlink-install` 指向的源码不受影响）并重新执行一次整个构建计划。非交互（CI、管道）或无可删目录时不询问，保持原失败与退出状态；验证命令失败不触发。真实验收：旧缓存引用 miniconda 的工作区，非交互直接失败不删除；答 `n` 保留目录、退出 1；答 `y` 删除后重建成功且 CMake 使用 `/usr/bin/python3`，源码完好。CLI 测试覆盖非交互时不询问、不删除。
- [x] 自定义 build / install / log base（2026-10-22 新增并完成）：路径依次取命令行透传、清单 `build_args`（`--x v` / `--x=v`，取最后一个）、colcon 默认文件 `build.build-base` / `build.install-base`、默认值，相对 workspace 目录；`--log-base` 是 colcon 全局参数（`colcon build --log-base` 会被拒绝，本机已核实），从参数中移到 `build` 之前，日志头按包路径依次取 `--log-base`、`$COLCON_LOG_PATH`、`log`。使用处：underlay 的 setup 在执行时依次查找配置与默认 base（同一次构建中先构建的 underlay 也能找到），均无则以“workspace 未构建”报错并列出查找路径；`rospm env` / `shell` 均无时跳过（首次构建前仍可进入）；Conda 旧缓存扫描、取消激活时的旧缓存检查与删除重试取第一个存在的目录，均无则不扫描 / 不询问；shebang 改写依次查找，均无时报错；`check` 的“已构建”同样依次查找。mixin 中设置的 base 不识别（代码中 `shortcut:` 注明）。测试：单元测试覆盖两种写法、最后一个生效、默认文件、绝对路径、去重；计划测试真实执行 underlay 查找的三种状态与 `env` 跳过、shebang 回退与报错、`--log-base` 前移与去重。
- [x] `rospm build [<workspace>] -- <colcon build 参数…>`（2026-10-22 新增并完成）：日常临时参数（`--packages-select`、`--parallel-workers`、`--cmake-args -DCMAKE_BUILD_TYPE=Debug` 等）不必写进清单，也不必绕过 rospm 直接运行 colcon 而失去日志、Conda 取消激活、旧缓存检查与删除重试。仅本次追加在所构建 workspace 的 `build_args` 之后、不写回清单（与 `--conda-env` 一致），colcon 同名参数取最后一个，因此命令行覆盖清单；只透传 `colcon build`，其它子命令在 `rospm shell` 中执行；`apply --build` 不接受透传，一键还原只依赖清单与锁。Conda 构建把解释器定义并入最后一组 `--cmake-args`（colcon 保留的那组）；透传的 base 参数优先级最高；未指定 workspace 时作用于每个 workspace（帮助与 `spec.md` 说明）；删除重试的确认提示写明删除整个目录、含本次未选择的包。测试：计划测试覆盖 `--cmake-args` 合并、`--log-base` 前移与日志路径；CLI 测试覆盖预览中的透传（含空格参数）、不写回清单、`apply --build` 拒绝透传。真实验收（本机 Humble）：`rospm build main -- --packages-select a_cpp --cmake-args -DCMAKE_BUILD_TYPE=Debug --build-base b2 --log-base lg` 只构建 a_cpp、`CMAKE_BUILD_TYPE=Debug`，使用 `b2` / `lg`，日志头指向 `lg/latest_build`，清单未变；`b2` 中引用 miniconda 的旧缓存被检出，删除重试提示列出 `b2` 与 `install`。
- [x] 在 `rospm-core` 定义统一诊断结构：稳定错误码、类别、严重程度、工具、文件 / 行 / 列、原始报错片段、关联诊断、解释与建议，并区分“工具已明确报告的事实”与“待验证的可能原因”。位置缺失时不编造行号；显示编译器报告的位置，不把它当作根因必然所在的位置。
  - [x] 已实现（2026-10-22）：公开 `diagnostic` 模块提供 `Diagnostic`、`Code`、`Category`、`Severity` 与 `Location`；机器枚举序列化为稳定 snake_case 标识，未知工具 / 位置为 `null`，行列使用可选的非零一基坐标。`raw` 保留原始片段，`related` 保留有序关联上下文；`explanation` 仅解释工具明确报告的事实，`possible_causes` 单列待验证假设，`suggestions` 提供排查建议，三者复用 `Message` 国际化。JSON 契约测试覆盖原始 ANSI、含空格路径、关联诊断、事实与假设分离、缺失位置 / 行列；本项仅定义共享结构，解析规则与 CLI 接入仍由后续条目完成。
- [x] C++ 首批规则：GCC / Clang 的语法与类型错误、缺少头文件；CMake 的 `find_package` 失败；链接器的 `cannot find -l...` / `undefined reference`。保留模板实例化和 include 链等关联上下文，区分语法、包发现、头文件搜索与链接问题；优先使用工具支持的结构化诊断，兼容常见文本输出与无法识别的格式。
  - [x] 已实现（2026-10-22）：`diagnostic::parse_cpp` 把构建日志（可含 ANSI）解析为按出现顺序的诊断，`raw` 保留原始行（含颜色）。编译器按 `文件:行[:列]: error|fatal error|note:` 识别（路径可含空格），语法 / 类型错误按消息前缀分类，其余编译错误为 `unknown` 但保留位置；缺少头文件识别 GCC `x.h: <strerror>`（strerror 随语言变化）与 Clang `'x.h' file not found`。include 链、`In function` / `In instantiation of` / `required from` 作为 `related` 挂到其后的错误，`note:` 挂到其前的错误，`N | …` 源码摘录并入 `raw`。CMake 取 `CMake Error at 文件:行 (命令):` 及缩进续行与 `Call Stack`，`find_package` 失败（配置文件模式与 `Could NOT find X` 模块模式）归为包发现并提取包名，其它 CMake 错误为 `unknown`。链接器识别 GNU ld / lld 的 `cannot find -l…`（含中文 `找不到 -l…`）、`unable to find library`、`undefined reference to`（`file:(.text+…)` 只给文件、带调试信息时给行）与 `undefined symbol`，`in function` 行作为关联上下文。警告、`collect2` / gmake 级联行不产出诊断（汇总留给 CLI 摘要）。解释 / 可能原因 / 建议的中英文文本已加入 locale。未采用 `-fdiagnostics-format=json` / SARIF：会替换用户实时看到的文本输出，故解析默认文本格式。Clang 摘录行没有 `|` 前缀，不并入 `raw`（代码中 `shortcut:` 注明）。测试：用 GCC 11.4 / ld 2.38 / CMake 3.22 的真实输出（含 ANSI、zh_CN 本地化、含空格路径）及 Clang / lld 文档格式覆盖各规则、模板链、note 归属、未知错误保留位置、成功日志与 OSC 超链接不误报。真实核对（本机 Humble，路径含空格、zh_CN）：一次 `colcon build --continue-on-error` 中缺分号、缺头文件、`find_package` 缺包、缺库、未定义引用五个包的合并日志，五条诊断类别、位置与名称均正确，无多余诊断。
- [x] Python 首批规则：`SyntaxError`、`IndentationError` / `TabError`、`ModuleNotFoundError`、常见 `ImportError` 及 traceback 位置；保留异常链与项目调用帧，对运行时异常按实际异常类型解释。模块缺失时检查本次构建 / 验证所用解释器，避免把“装在另一个环境”误报为完全未安装。
  - [x] 已实现（2026-10-23）：核心库 `diagnostic::parse_python(output, interpreter)` 解析默认 Python 文本输出，覆盖语法、缩进、制表符、模块缺失、导入失败及运行时异常；解释保留实际异常类型与消息，调用方可传入本次构建 / 验证命令的解释器或环境标识。文件与行号取工具报告，路径含空格与 Unicode 均保留；caret 摘录不换算列号（缩进与制表符展开有歧义），无位置时不编造。`raw` 保留 ANSI、源码和 caret，`related` 保留有序调用帧；显式 `raise … from …` 与隐式异常链均关联到最终异常，独立失败不串联。导入错误建议在失败命令环境中核查 `sys.executable`、`sys.path` 与模块导入，并检查循环导入、同名遮蔽或扩展 ABI；可能原因与事实分离，不断言模块全局未安装，不把模块名当作安装包名。解析器不启动解释器，实际环境检查作为建议交给调用方 / 用户；CLI 接入仍属后续待办。中英文 locale 已补齐。测试覆盖 ANSI、含空格路径、异常链、缺失 / 零行号、自定义异常、常见 ImportError、成功日志和命令边界；真实运行隔离的 `python3 -I -S`，验证 SyntaxError、IndentationError、TabError、ModuleNotFoundError、ImportError、ZeroDivisionError 及 KeyError → ValueError 链。按单命令 / 包日志解析，不尝试拆解交错 traceback；ExceptionGroup 树等未知格式留在原始日志。
- [x] 依赖建议结合 `package.xml`、rosdep、项目清单及当前提供者：有明确映射和检查证据时提示缺少的依赖及对应还原 / 安装步骤；无法确认时提示“未找到头文件 / 模块，可能缺少依赖或搜索路径未配置”，不把 Python 模块名直接当作 pip / Conda 包名，不把所有链接错误归为缺包。诊断提供排查建议，不自动修改源码或安装依赖。
  - [x] 已实现（2026-10-23）：核心库 `diagnostic::suggest_dependencies` 在已解析诊断上追加、去重建议，递归覆盖异常链；复用 `PackageXml`、项目 `Manifest`、native `DepsReport` / `EnvReport`，不执行探测、安装或源码修改，不改写工具事实与原始日志。头文件、CMake 包、链接库与 Python 模块只接受调用方提供的精确 `DependencyMapping`（需带文件索引 / 上游元数据等来源）；rosdep 只证明键到安装包的映射，不能证明模块 / 文件归属，不内置猜测映射。仅映射、有效声明与 Missing 检查同时存在时建议审阅并运行 `deps install` 或 `apply`；多包 rosdep 解析只称“未全部安装”，不误称每个包均缺失。无映射 / 检查、冲突检查、已安装、禁用 rosdep、skip_keys、条件不成立、未知包归属和项目内依赖均提供对应排查建议；CMake 同名声明仅作为待检查键。非 native 提供者不套用主机 apt 证据，Python 仅在调用方确认系统解释器且未声明 Conda 时使用主机缺包证据，其他环境提示核对实际解释器 / Conda 环境文件与锁；`dependencies.pip` 明确仅记录，不从模块名生成安装命令。未定义符号不追加缺包结论。中英文文本已补齐；测试覆盖各证据门槛、提供者 / Python 环境隔离、项目内依赖、部分安装集合、重复调用、异常链、目标平台 apt 替换 / 排除及原诊断不变。此项完成建议层，映射证据与报告由调用方传入，自动采集及 CLI 展示仍在后续接入范围内。
- [x] CLI 在失败后输出简短摘要，按包归组、合并重复诊断并优先展示直接报错，将 colcon / make 的失败汇总关联为后续结果；不将日志中的第一个错误武断视为唯一根因。示例：`src/main.cpp:42:10：编译器报告此处缺少分号；请检查此处及前一条语句。`、`节点使用的 Python 环境中未找到模块 numpy；请核对项目依赖和环境配置。` 保留完整日志路径，解释与建议走中英文 i18n；`--json` 输出稳定字段，原始子进程输出不混入 JSON 数据流。
  - [x] 已实现（2026-10-24）：`build` / `apply --build` 失败后解析当前失败命令，优先读取本次更新的 colcon 按包日志，按 workspace / 包分组；重复诊断合并并保留关联上下文，make / colcon 结果单列为后续信息，不断言唯一根因。终端最多展示 8 组、8 条直接诊断及首条建议，始终提示完整日志；JSON 保留稳定字段、完整已解析诊断与关联上下文，`apply` / `build --json` 的计划及子进程输出转 stderr，stdout 只输出一个结果对象，JSON 模式不询问清理重试。测试覆盖中英文、多包、重复与级联、Python 环境、旧包日志和先前命令隔离、成功不输出旧摘要、JSON 可解析及原失败状态。每份日志最多解析 4 MiB，完整日志不裁剪；未识别格式的进一步兜底及真实工具端到端验收仍见下两项。
- [x] 未识别错误时保留原始报错并提示查看日志；解析失败不得覆盖原命令失败状态，不得将失败改为成功。同步命令帮助、日志位置、诊断覆盖范围和 JSON 约定。
  - [x] 已实现（2026-10-24）：未知格式保留实时原始输出及日志（含 ANSI），中英文提示查看日志；日志缺失、不可读或读取范围内缺少失败命令边界时仍返回摘要和日志路径，不覆盖原命令错误。JSON 新增稳定 `diagnostic_status`（`parsed` / `unrecognized` / `unavailable`），空诊断不代表成功，CLI 仍退出 1 并保留子命令状态。同步 `build` / `apply --build` 中英文帮助与 `spec.md` 的覆盖范围、日志位置及 JSON 契约；单元与 CLI 测试覆盖未知格式、缺失日志 / 命令边界、ANSI 原文、中英文提示及 JSON 失败状态。
- [ ] 验收：用 GCC / Clang、CMake / 链接器及 Python 的真实失败样例验证位置、类别和建议；覆盖多包并行、重复与级联错误、模板上下文、异常链、路径含空格、ANSI 颜色、缺失位置、未知格式与成功日志不误报。增加最小 C++ / Python 失败项目的 CLI 集成验收，验证实时输出、完整日志、中英文摘要、JSON 可解析及失败退出状态。
- **产出**：常见 C++ / Python 报错可定位、可解释并提供有依据的排查建议；未知错误仍可追溯完整日志，CLI / GUI 共用诊断结果。

---

## Phase 4 — Release 与 schema v1

状态：**未开始**。

- [ ] 完成 aarch64 与 Phase 3.5 Conda 自动配置、一键还原验收后的回归测试、文档与示例清单整理。
- [ ] 完成 Phase 3.6 常见错误诊断验收，确认原始日志、失败状态与机器可读输出不受影响。
- [ ] 冻结 manifest / lock schema v1，整理迁移说明与兼容性声明。
- [ ] 构建并发布 1.0 版本及 aarch64 产物。
- **产出**：1.0 发布，native 提供者、aarch64 目标与 schema v1 作为稳定基线。

---

## Phase 4.5 — 完整 TUI 与独立任务执行

状态：**未开始**。排在 Phase 4 稳定基线之后、Phase 5 离线导出与导入之前。工期按一名熟悉 Rust 的开发者、Linux 常见终端与现有业务能力估算，先用执行层原型校准，不作为发布承诺。

目标：提供“环境概览与差异 → 审阅计划 → 执行 → 查看日志与诊断 → 处理失败”的完整终端工作流，支持键盘与鼠标、本机终端与 SSH。CLI 保留脚本 / CI 入口，TUI 复用同一业务逻辑、校验、确认与诊断，不通过解析 CLI 的人类可读输出判断任务状态。

架构边界：`rospm-core` 保持业务库；独立、按需启动的用户级 worker 持有任务与子进程，通过本机 Unix socket 与前端通信，可复用同一可执行文件的内部入口。TUI 关闭、卡死或崩溃不等于取消任务；worker 在任务结束并保存结果后退出。首期不引入常驻 daemon、远程网络协议、内嵌终端模拟器或任意步骤自动续跑。SSH 用户登录目标机器后在该机器运行 TUI；只有需要开机恢复、无人登录调度或长期共享任务时，再评估 systemd 用户服务 / daemon。

### 4.5.1 统一执行与交互接口

- [ ] 梳理 CLI 命令、provider、Conda 与计划执行中的直接终端读写；复用现有 `Plan` / `Command`，提供结构化任务状态、步骤进度、日志与确认接口。复用 Phase 3.6 诊断结果，机器字段稳定，显示文本走中英文 i18n。
- [ ] 将换源、卸载、清理构建目录后重试等确认流程从 CLI 展示中分离，CLI / TUI 共用同一业务规则；确认绑定具体任务、步骤与操作范围，清理前展示完整目录及影响，断连时不默认同意。
- [ ] 定义任务成功、失败、等待确认、取消请求与已取消的状态及退出结果；明确取消的进程组范围和安全时机，安装等不可任意中断的步骤不承诺即时取消或事务回滚。
- [ ] 保留 CLI 参数、退出码、`--dry-run` 无副作用、非交互确认规则与 JSON 数据流约定；新增接口覆盖现有调用者、测试与 fixtures，不复制一套执行逻辑。
- **产出**：CLI 通过共享接口完成原有操作并通过回归；执行状态与交互请求可供 TUI 消费，不依赖终端文本解析。

### 4.5.2 按需 worker 与任务重连

- [ ] worker 以当前用户执行，独立持有子进程与任务生命周期；验证会话 / 进程组、SIGHUP、继承文件描述符及 SSH 登出清理策略，不能把普通 spawn 当作脱离终端。先以原型验证 TUI 被杀与 SSH 断开后任务继续的条件，记录支持范围。
- [ ] Unix socket 放在用户私有目录，限制权限并校验对端身份；定义最小协议版本、任务标识、状态快照、日志游标与确认响应。不提供任意 root 命令执行接口，不允许重连隐式重复提交任务。
- [ ] worker 独立保存每个任务的日志与最终结果；重连时读取快照和增量日志，任务结束、worker 退出后仍可查看结果。明确日志保留 / 清理规则及现有 `.rospm/log/build.log` 的兼容方式，不能将每次覆盖的日志直接当作任务历史。
- [ ] 持续读取子进程 stdout / stderr，日志落盘与 UI 发送解耦；限制内存和发送队列，慢客户端、断连或 socket 写入失败不能堵住子进程。日志写入失败须报告，不静默声称日志完整。
- [ ] 同一工作区的冲突修改互斥，尊重 apt / dpkg 等工具自身的系统锁；退出界面与显式取消分开。需要确认但没有客户端时安全等待，并允许重连处理或取消。
- [ ] 区分“UI 断开后继续”“SSH 断开后继续”和“worker / 主机故障后恢复”；worker 异常留下的任务标记为中断或状态待核实，不自动重新执行安装、清理等有副作用步骤。
- **产出**：TUI 卡死或被杀不影响正在执行的任务；支持范围内 SSH 断开后可重新连接并查看准确状态、日志与结果，无重复执行、管道阻塞或无界缓存。

### 4.5.3 TUI 工作流、权限与 shell

- [ ] 增加 `rospm tui` 入口及独立 TUI 模块 / crate，优先评估 Ratatui + Crossterm；非终端环境给出 CLI 使用提示。保留现有 CLI 入口与单文件分发方式。
- [ ] 提供项目 / workspace 与环境概览、check / verify / deps 结果、计划预览、任务状态、日志浏览及错误诊断；接入现有已实现操作，未实现的 provider 或命令明确标注，不扩大本阶段的业务范围。
- [ ] 支持选择 workspace、构建参数与 Conda 环境、审阅 apply / uninstall 计划、失败后的清理确认与重试；临时参数沿用 CLI 语义，不静默写回清单。首期不做通用清单编辑器。
- [ ] 完整键盘操作与可见快捷键、鼠标点击 / 滚动、明确焦点和弹窗边界；覆盖中文宽度、窗口缩放、窄窗口、鼠标与文本选择冲突。日志按批刷新、只绘制可见区域，渲染与执行互不阻塞。
- [ ] 仅系统步骤提权，不以 `sudo rospm` 作为默认使用方式；认证时暂停 TUI、恢复终端，由 sudo 直接读取密码，再恢复界面。结合 worker 的终端归属验证认证路径，处理 sudo 缓存过期 / 不适用与无人连接时等待授权，不假设一次 `sudo -v` 永久有效；Git、Conda、构建与项目 shell 保持用户身份。
- [ ] `shell` 暂停绘制与输入捕获，关闭 raw mode / 鼠标模式并退出 alternate screen，将前台终端交给现有 Bash 子进程；退出后恢复 TUI、重新探测项目状态。正确处理前台进程组与 Ctrl-C；后台任务仍由 worker 管理。首期不在 TUI 面板内嵌 PTY / 终端模拟器。
- **产出**：键盘与鼠标均可完成从计划审阅到失败处理的闭环；sudo 与 shell 不破坏界面或扩大项目操作权限，CLI / TUI 的业务结果一致。

### 4.5.4 集成验收与发布收尾

- [ ] 执行接口与协议测试覆盖状态迁移、确认绑定、重复提交防护、工作区互斥、取消进程组、客户端断连与重连、最终结果持久化；CLI 原有行为与中英文 / JSON 输出回归通过。
- [ ] 真实故障验收：暂停 / 杀死 TUI、大量 stdout / stderr、慢客户端、日志写入失败、worker 异常退出、SSH 断开；验证内存有界、子进程不因 UI 阻塞、结果不误报、重连不重复执行。
- [ ] 在隔离环境验收系统安装中的退出 / 取消、sudo 认证失败与过期、等待确认时断连、清理目录后重试，确保不默认同意破坏性操作、不自动重放中断任务。
- [ ] 制定并实测终端矩阵：常见本机终端、SSH、tmux，覆盖中文、鼠标、窄窗口、缩放、低带宽 / 高延迟；测试 shell 中运行交互程序、Ctrl-C 及退出后的界面恢复，记录实际支持范围与限制。
- [ ] 同步命令帮助、README、spec、决策与路线图；说明任务日志位置及保留规则、退出 / 取消区别、提权方式、SSH 存活条件和故障恢复边界。
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
