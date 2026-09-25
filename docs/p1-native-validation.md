# P1 native 提供者实机验证（Ubuntu 22.04 / Humble）

验证日期：2026-09-26。环境：WSL2 上的 Ubuntu 22.04.5（x86_64，12 核 / 15 GB），sudo 免密，
起始状态无 `/opt/ros`、无 ROS apt 源、无 vcs / colcon / rosdep，`universe` 已启用，locale 为
`zh_CN.UTF-8`。验证项目为 `examples/minimal-humble` 的副本，放在 `/tmp/rospm-p1`，
全部命令输出保存在 `/tmp/rospm-p1/log/`（`00`–`20` 编号，本文引用的编号即文件前缀）。

**结论：phases.md P1 验收的 22.04 三条全部通过。** 过程中发现并修复 4 个缺陷、补 1 项探测
（见 §5），24.04 + Jazzy 同流程待另一台机器。

## 1. 基线（log 00–03）

`rospm check` 报告 9 项 `MISSING`（ROS apt 源、vcs / colcon / rosdep、`/opt/ros/humble`、
`ros-humble-ros-base`、rosdep init / update、源码未 import），`--strict` 退出 1；
`doctor` 报 core tools missing。`apply --dry-run` 生成 11 步计划，从 `apt-get update` 与
写 `ros2.list` 起步，与 CI 冒烟的断言一致。

## 2. `apply`（log 04–08）

**第一次（04）失败**：第 3 步 `sudo curl -sSL … ros.key -o …keyring.gpg` 挂起。`ss -tnpi`
显示该 TLS 连接处于重传退避（`cwnd:1 backoff:7 retrans:1/8`），而同一时刻手动 `curl`
（含 `sudo`）0.6 s 内返回 200，即偶发网络停滞，与本环境"HTTP/2 到 GitHub 不稳定"的既有
观察一致。原实现没有 `-f` / 超时 / 重试，会无限等待；手动 `kill -TERM` 后 rospm 正确
报 `command failed … (signal: 15)` 并以退出码 1 结束，keyring 与 `ros2.list` 都未写入。

**第二次（05）成功**，11 步全部执行，耗时约 6 分钟（其中 `ros-humble-ros-base`
及工具 2 分钟、`rosdep install` 拉 OpenCV 等约 3 分钟）：

- `vcs import` 把 ros2/demos@humble 克隆到 `ws/src/demos/demo_nodes_cpp`（整个 demos 仓库，21 个包）；
- `sudo rosdep init` 写出 `20-default.list`，`rosdep update --rosdistro humble` 只加载 humble；
- `rosdep install … -y` 解析 21 个包，装 `ros-humble-example-interfaces` 与 OpenCV 等系统依赖，结束于 `#All required rosdeps installed successfully`。

之后 `check --strict` 退出 0（17 项全部 `ok`，仅 workspace 未构建为 `info`），`doctor`
报 humble / apt 源 / rosdep 均就绪。**再次 `apply --dry-run`（08）** 计划缩为 7 步：
locale / universe / 密钥 / 源文件步骤因探测已满足而跳过并注明"already configured"，
`rosdep init` 跳过；保留的 `apt-get install` / `vcs import` / `rosdep update|install`
由工具自身保证幂等。

## 3. `build` / `env` / `shell`（log 09–13、15–16）

- `build`：`(cd ws && source /opt/ros/humble/setup.bash && colcon build --symlink-install)`，
  21 个包 2 分 40 秒完成，`ws/install/setup.bash` 出现，`check` 中 workspace 行变为 `built`。
- `env`：输出 `source /opt/ros/humble/setup.bash`、带存在性保护的 workspace `setup.bash`、
  `export ROS_DOMAIN_ID=0`。
- `shell`：通过 stdin 喂入命令验证。提示符前缀 `(rospm:minimal)`、`ROSPM_PROJECT` /
  `ROS_DOMAIN_ID` / `ROS_DISTRO` 正确，`AMENT_PREFIX_PATH` 按 workspace 21 个包叠在
  `/opt/ros/humble` 之前，`ros2 pkg prefix demo_nodes_cpp` 指向 `ws/install/demo_nodes_cpp`，
  `ros2 run demo_nodes_cpp talker` 正常发布 `Hello World: 1..3`（由 `timeout 4` 终止）。
- `[workspaces.main.colcon]` 各字段（用同目录下 `rospm-defaults.toml` / `rospm-mixins.toml`
  两份派生清单）：
  - `defaults = "colcon-defaults.yaml"` → 计划中出现 `COLCON_DEFAULTS_FILE=…`，
    `packages-select` 使 colcon 只构建 `demo_nodes_cpp_native`，`cmake-args` 的
    `-DCMAKE_BUILD_TYPE=Release` 出现在 CMakeCache。
  - `mixins = ["release"]`：apt 装的 `python3-colcon-common-extensions` **不含** `colcon mixin`
    子命令，colcon 报 `unrecognized arguments: --mixin release`，rospm 退出 1。手动
    `apt-get install python3-colcon-mixin` + `colcon mixin add default … && colcon mixin update`
    （decision #8 明确这属于用户环境，rospm 不还原）后，`--mixin release` / `--mixin debug`
    分别把 `CMAKE_BUILD_TYPE` 切到 Release / Debug。`colcon mixin update` 在本环境也曾
    挂起 5 分钟，限时重试后成功。

## 4. `uninstall`（log 17–19）

- `uninstall --dry-run`：显式列出 197 个 `ros-humble-*` 包，随后 `autoremove`、
  `rm -f ros2.list keyring`、`apt-get update`，并注明工具保留与 `/opt/ros/humble` 可能残留。
- 无 `--yes` 且 stdin 非终端：打印计划后拒绝执行，退出 1。
- `uninstall --yes`：约 19 秒完成。`autoremove` 又回收 70 个包。之后：
  - `check`：ROS apt 源、`/opt/ros/humble`、`ros-humble-ros-base` 三项 `MISSING`，
    `--strict` 退出 1；`doctor` 报 `none found under /opt/ros`。
  - `/opt/ros` 目录整个消失（本例下 apt 拥有全部文件，不需要手动清理）；`ros2.list`、
    keyring 已删；`20-default.list` 与 `~/.ros/rosdep` 缓存按设计保留。
  - 与基线相比仍多 263 个包、0 个 `ros-*`：vcs / colcon / rosdep 工具（设计保留），以及
    rosdep 代为安装的系统依赖（`libopencv-*` 41 个、`python3-*` 50 个、cmake、graphviz 等）。
    这些包由 rosdep 显式安装，apt 不视为自动安装，`autoremove` 不会回收。**这是 decision #15
    已接受的范围**，但用户会感知；若要更彻底，需要在 `apply` 时记录 rosdep 装了什么
    （P2 锁文件的 apt 层可以顺带做）。

## 5. 修复与改动

| # | 现象 | 处理 |
|---|---|---|
| 1 | 密钥下载无超时，网络停滞时 `apply` 永久挂起；`curl` 无 `-f`，404 时会把 HTML 当 keyring 写入；未校验指纹 | 新增 `Step::FetchKey`：下到临时文件（`curl -f`、连接 15 s、单次 60 s、瞬时错误重试 2 次），`gpg --show-keys --with-colons` 解析主密钥指纹并与内置 `ROS_KEY_FINGERPRINT` 比对，通过后 `sudo install -m 0644` 到 keyring。前置包加 `gnupg`（ubuntu 容器镜像只有 `gpgv`）。指纹解析有单测；下载正确密钥 / 拒绝错误指纹 / 404 立即失败三个 `#[ignore]` 测试本机实跑通过（log 14） |
| 2 | `--manifest rospm.toml`（不带目录的相对路径）报 `cannot resolve : No such file` | `load()` 对空 parent 回退到 `.`，错误信息补充"project directory" |
| 3 | 错误文本 `command failed: … (exit status exit status: 2)` 重复 | `Error::CommandFailed` 格式改为 `({status})` |
| 4 | `fetch_key` 临时文件名只含 pid，同进程内多次调用（含并发测试）互相覆盖 | 文件名加进程内自增序号 |
| 5 | 清单声明 `mixins` 但主机缺 `colcon mixin` 时，直到 `build` 才失败 | `check` 在清单使用 mixins 时探测 `colcon mixin show`，缺失报 `MISSING` 并给出安装与 `mixin add/update` 提示 |

另有两处观察未改：`examples/minimal-humble/main.repos` 的条目名 `demos/demo_nodes_cpp`
实际克隆的是整个 ros2/demos（21 包，构建 2 分 40 秒），与"minimal"的意图不符，若只想要
demo_nodes_cpp 需要 sparse checkout 或改用 `COLCON_IGNORE`；`examples/ros2_demos/rospm`
是一个被 git 跟踪的 66 MB 二进制，应从仓库移除并加入 `.gitignore`。

## 6. 第二轮闭环（log 20a / 20 / 21–23）

用修复后的二进制在"卸载后"的主机上再跑 `apply`（此时 `rosdep init` 已存在、`/opt/ros`
不存在、无 apt 源），目的是验证新的密钥步骤与残留状态下的幂等，随后 `uninstall --yes`
让主机回到干净状态。三次尝试，结果受本机到 GitHub 的连通性限制：

- **20a**：第 3 步 `curl` 在 `--max-time 60` × 3 次尝试后以退出码 28 失败，`apply` 退出 1，
  总耗时 3 分 06 秒；keyring 未写入，`/tmp/rospm-key-*` 临时文件已清理。这正是修复前会
  永久挂起的场景，现在有界失败。
- **20 与 21（同样结果）**：`[2] apt-get install … gnupg` → `[3] fetch … verify fingerprint`
  通过（keyring root 0644，`gpg` 读出的指纹与内置常量一致）→ 写 `ros2.list` → 装底包 →
  `vcs import`（已有 checkout 被 vcstool 跳过）→ **`[9] rosdep update` 失败**：rosdep 自身
  15 秒读超时 `ERROR: error loading sources list: The read operation timed out`，rospm 以
  退出码 1 报告失败命令，第 10 步未执行。手动 `rosdep update` 与 `curl -m 20` 访问
  raw.githubusercontent.com 同样超时（0 字节），确认是主机网络而非 rospm；第 9–11 步的
  代码与 §2 中成功的第二次 `apply` 完全相同。此时 `check --strict` 仍退出 0，是因为
  前几轮留下的 rosdep 缓存与依赖还在，不能作为本轮成功的证据。
- 两次之后的 `uninstall --yes` 都成功：191 个 `ros-humble-*` 移除，`check` 三项 `MISSING`，
  `/opt/ros`、`ros2.list`、keyring 均不存在（log 23）。
- 用派生清单 `rospm-mixins.toml` 跑 `check`，新增的 `colcon mixin` 行为 `ok`（此时
  `python3-colcon-mixin` 已装）。

因此：新二进制的 bootstrap（第 1–8 步，含真实指纹校验）与 `uninstall` 有实机证据；
第 9–11 步的实机证据来自 §2（旧二进制，代码未变）。等网络恢复或在 24.04 实例上跑
同一流程时，应再做一次不中断的完整闭环。

主机最终状态：无 ROS、无 ROS apt 源；保留 vcs / colcon / rosdep / colcon-mixin 工具、
`rosdep init` 与缓存、rosdep 装的系统库；`/tmp/rospm-p1/ws` 中的源码与构建产物（约 500 MB）
与 `/tmp/rospm-p1/log/` 保留供复查。

## 7. 源镜像与网络测速（决策 #17，log 24–30）

会话中期用户提出：下载前先测速，官方源不通时引导换到国内镜像。当时本机到 GitHub 正好持续不通，
按 §6 的方式无法完成闭环，于是在这台机器上实现并实测了该功能。

- **镜像布局核实**（`curl`，全部 200）：TUNA / USTC / 阿里云都提供 `/ros2/ubuntu`；TUNA 与 USTC
  还镜像 `rosdistro/`（`ros.key`、`index-v4.yaml`、`rosdep/*.yaml`）；上交大无 `/ros2`。镜像里的
  `20-default.list` 内容仍指向 GitHub，因此换源时 rosdep 源列表必须由 rospm 自己写。
- **`doctor`**：新增测速表，本机结果：官方 apt 522 ms 可达，官方 rosdistro 8 s 超时；
  tuna / ustc 的 apt 与 rosdistro 均 100–300 ms（`base.yaml` 约 1 MB/s），aliyun apt 374 ms。
- **`apply --dry-run`** 不测速（离线可用），`--mirror tuna --dry-run` 打印镜像版计划：
  `ros.key` 改从 TUNA 取、`ros2.list` 指向 TUNA、写 `20-default.list`、`rosdep update` 带
  `ROSDISTRO_INDEX_URL`（log 24）。
- **非交互 `apply`（auto）**：探测瞬间 GitHub 恰好可达（717 ms）→ 判定官方可用 → 正常执行到
  第 8 步，`rosdep update` 又在 GitHub 上超时。由此把 rosdistro 探测对象从 5 KB 的
  `index-v4.yaml` 换成 270 KB 的 `rosdep/base.yaml`（测真实吞吐、暴露中途停滞），并让官方源下
  的步骤失败时提示 `rospm doctor` 与 `--mirror`。
- **交互提示**（伪终端，log 27）：官方 rosdistro FAILED → 列出候选 `ustc, tuna`（按延迟）→
  `use the ustc mirror … ? [Y/n]`，回答 `n` 后中止、退出 1、未执行任何步骤。
- **`apply --mirror tuna` 第一次**（log 25）：源文件由官方重写为 TUNA，密钥从 TUNA 下载且指纹
  一致，底包从 TUNA 安装，但 `rosdep update` 退出 1：上游 `20-default.list` 的
  `gbpdistro …/releases/fuerte.yaml` 行会让 rosdep 从硬编码的 GitHub 地址取 `targets.yaml`。
  该行是 ROS 1 fuerte 遗留，ROS 2 不需要，镜像版列表去掉了它。
- **`apply --mirror tuna` 第二次**（log 28，卸载后的干净起点）：13 步全部成功，3 分 05 秒，
  `rosdep update` 与 `rosdep install` 均走 TUNA，`#All required rosdeps installed successfully`；
  `check --strict` 退出 0，ROS apt 源一行显示 `(tuna)`；随后 `uninstall --yes` 成功，
  `check` 三项 `MISSING`，`/opt/ros`、`ros2.list`、keyring 均不存在（log 30）。
  这同时补上了 §6 未能完成的"新二进制完整闭环"。

主机最终状态与 §6 相同，另外 `/etc/ros/rosdep/sources.list.d/20-default.list` 现在是 rospm 写的
TUNA 版本（rosdep 属于保留工具，`uninstall` 不动它）。
