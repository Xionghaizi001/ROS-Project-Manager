# P2 `apply --locked` 验证记录

本文按时间点记录 Phase 2 剩余验收（`apply --locked` 的真实可用性、两机 / 间隔数周一致性）的进展。
每个时间点用同一个脚本 `scripts/p2-locked-sandbox-check.sh` 采集，便于逐项对比。

脚本**不修改系统**：不写 `/etc/apt`、不装包、不用 sudo。它做三件事：

1. 用本地 git 上游建临时项目，真实联网执行 `check` / `doctor` / `apply --dry-run` / `lock` / `verify` / `deps check` / `apply --locked --dry-run`。
2. 在隔离的 apt 目录（`APT_CONFIG` 指向临时 `Dir::State` / `Dir::Cache` / `SourceList`）中配置锁文件记录的 ROS 与 Ubuntu 快照，`apt-get update` 后用 `apt-cache madison` 逐条核对锁定版本能否从快照下载，并与实时 ROS 源的底包版本对比。
3. 按"在快照日装好 ROS 的机器"合成一份完整锁文件，生成 `apply --locked --dry-run` 计划，再用 `apt-get -s` 模拟计划里的锁定安装命令：一次用空 dpkg 状态（干净机器），一次用本机 dpkg 状态加本机实时 Ubuntu 源（真实 `apply --locked` 时 apt 看到的源集合）。

真实的 `apply --locked` 仍须在干净虚拟机上执行（本机约定不做真实 `apply`，见 phases.md P1 / P2）。

```bash
cargo build --release
scripts/p2-locked-sandbox-check.sh /tmp/rospm-p2-check-<标记>   # 约 1 分钟，需联网
# 结果：<目录>/log/summary.txt 与编号日志
```

发行版由 `/etc/os-release` 推导：22.04 → humble，24.04 → jazzy。

---

## 时间点 1：2026-09-26，Ubuntu 22.04 / Humble（WSL2，x86_64）

主机状态：无 `/opt/ros`、无 ROS apt 源；vcs / colcon / rosdep 工具与 `rosdep init` / `update` 缓存在（P1 卸载后按设计保留）；`python3 -m pip` 不可用。
日志：`~/rospm-validation/p2-t1-2026-09-26-jammy/`（脚本）与 `…-manual/`（逐步手工过程）。

### 汇总（`summary.txt`）

```
time (UTC)                                   2026-09-26T10:06:19Z
host                                         Ubuntu 22.04 jammy amd64, rospm 0.0.1 @ “project: remove unexpected fiile”（2026-09-26 本地历史改写前为 35e8cda）+ 本文修复
rospm check                                  exit 0
rospm doctor                                 exit 0
rospm apply --dry-run                        exit 0
rospm lock (network)                         exit 0
  ROS snapshot / Ubuntu snapshot             2026-08-07 / 20260808T004205Z
rospm verify                                 exit 0
rospm deps check                             exit 0
rospm apply --locked --dry-run (this lock)   exit 1
sandbox apt-get update (snapshots)           exit 0
sandbox apt-get update (host+snapshots)      exit 0
locked versions not in snapshot (this lock)  2 / 4
  NOT-IN-SNAPSHOT  curl=7.81.0-1ubuntu1.29  (snapshot has: 7.81.0-1 7.81.0-1ubuntu1.25 )
  NOT-IN-SNAPSHOT  python3-rosdep=0.27.0-1  (snapshot has: 0.26.0-1 )
ros-humble-ros-base live / snapshot          0.10.0-1jammy.20260908.101203 / 0.10.0-1jammy.20260804.204550
newest ROS snapshot dates                    2026-05-14 2026-07-02 2026-08-07
sim: clean install from snapshot             exit 0, 645 pkgs
apply --locked --dry-run (synthetic lock)    exit 0
  update between snapshot write and install  yes
sim: locked install, clean machine           exit 0, 645 pkgs
sim: locked install, this host (+live Ubuntu) exit 100, 0 pkgs
   curl : Depends: libcurl4 (= 7.81.0-1ubuntu1.25) but 7.81.0-1ubuntu1.29 is to be installed
```

### 命令可用性

| 命令 | 结果 |
|---|---|
| `check` / `doctor` | 可用。未装 ROS 时报 ROS apt 源、`/opt/ros/humble`、底包 `MISSING`。`doctor` 测速表显示官方 ROS apt 可达，`raw.githubusercontent.com` 8 秒无响应，三个镜像均可达。 |
| `apply --dry-run` | 可用，10 步计划，与 P1 一致。 |
| `lock` | 可用，真实联网约 5 秒。humble 最新快照仍是 `2026-08-07`，ROS 与 Ubuntu 快照签名均验证通过。底包未安装时不记录并提示先 `apply`。pip 不可用时只说明。 |
| `verify` | 可用，锁文件记录的 4 个 apt 包与 1 个源码仓库全部 ok，退出 0。 |
| `deps check` | 可用，经真实 rosdep 把 `rclcpp` / `std_msgs` / `ament_cmake` 映射为 `ros-humble-*` 并报 `MISSING`，`libopencv-dev` 已装。 |
| `apply --locked --dry-run`（本机锁） | 按 spec 报错退出 1：锁文件未固定底包 `ros-humble-ros-base`。 |
| `apply --locked --dry-run`（合成锁） | 可用，15 步：快照 keyring 按指纹下载、`ros2.list` 指向 `snapshots.ros.org/humble/2026-08-07`、写 `rospm-ubuntu-snapshot.list`、`apt-get update`、两条 `--allow-downgrades` 安装（195 个包，每条不超过 100）、`git checkout --detach`、`rosdep update`，跳过 `rosdep install`。 |
| 真实 `apply` / `apply --locked` | **未执行**。本机约定不做真实安装，本次尝试被权限策略拦下。 |

### 发现

1. **已修复：写 Ubuntu 快照源后缺少 `apt-get update`。**
   原计划在写 `rospm-ubuntu-snapshot.list` 之后直接执行锁定安装，apt 只看得到写文件之前抓取的实时源索引。实时 `-updates` / `-security` 只保留最新版本，锁定的旧版本因此装不上。现在写文件后紧跟一条 `apt-get update`，`tests/plan.rs` 增加断言：快照文件写入与第一条锁定安装之间必须有 update；去掉修复时该测试失败。

2. **未修复（设计问题）：`lock` 记录的版本可能不在所选快照里。**
   实时 ROS 源的 `ros-humble-ros-base` 构建于 2026-09-08，最新 humble 快照是 2026-08-07。今天从实时源安装的机器执行 `lock`，记录的 `ros-humble-*` 版本在任何快照中都不存在；另一台机器 `apply --locked` 会在安装步骤失败。Ubuntu 层同理：Ubuntu 快照时间戳由 ROS 快照的 `Release` 日期换算（2026-08-08），本机之后更新的 `curl …1ubuntu1.29` 不在其中；ROS 源里的 `python3-rosdep 0.27.0-1` 也晚于快照（快照只有 `0.26.0-1`）。
   建议：
   - `lock` 对每条记录的版本核对所选快照的 `Packages`，缺失时把快照标为 DEGRADED 并列出包名，而不是静默写入一份无法回放的锁；
   - Ubuntu 快照时间戳改用 `lock` 执行时刻（`snapshot.ubuntu.com` 接受任意时间戳），不再从 ROS 快照换算；
   - 在 spec 中写明可复现的工作流：先让机器 A 从快照安装（例如为 `apply` 增加从最新快照安装的选项），再 `lock`。否则只能等 ROS 发布新快照后再 `lock`。

3. **未修复（设计问题）：锁文件只记录顶层 Ubuntu 包，不记录其精确依赖。**
   合成锁在干净机器上能完整解析（645 个包）。但在本机这样系统库比快照新的主机上，`curl=7.81.0-1ubuntu1.25` 要求同版本的 `libcurl4`，apt 不会自动降级未点名的包，整条安装命令失败（退出 100，一个包都不装）。去掉 `curl` 后 ROS 包可以解析，`python3-rosdep` 能按锁降级（`--allow-downgrades` 生效），但 `libssl-dev` 等 ROS 依赖的 Ubuntu 库会取实时源的新版本，Ubuntu 层只是部分复现。
   建议：非 `ros-*` 的 Ubuntu 包要么连同依赖闭包一起记录，要么在 `apply --locked` 中按 spec §3.2 的降级规则"只校验版本、不强制重装"，避免单个包拖垮整条命令。

4. **小问题：`verify` 不检查锁文件是否含底包。** 锁文件缺底包、主机也没装 ROS 时，`verify` 仍报"就绪"并退出 0，因为它只比较锁文件里有的条目。`apply --locked` 对同一情况会报错。建议 `verify` 同样把"锁文件缺清单底包"报为问题。

5. **环境事实。** `packages.ros.org` 与 `snapshots.ros.org` 的 HTTPS 证书与域名不符（前者是 `*.osuosl.org`，后者是 `*.cloudfront.net`），只能走 HTTP。rospm 对这两个域名一律使用 `http://`，不受影响。

### 真实闭环（同日，经用户同意在本机实装）

用户明确同意在本机真实安装后，用临时项目 `/tmp/rospm-p2/real` 跑了两轮闭环。该项目的源码是本地 git 上游，含一个依赖 rclcpp / std_msgs 的最小节点。日志在 `~/rospm-validation/p2-real-2026-09-26-jammy/`。

**第一轮：实时源。** 每一步都成功。

| 步骤 | 结果 |
|---|---|
| `apply --mirror tuna` | 14 步成功，33 秒 |
| `check --strict` | 退出 0 |
| `build` 与运行节点 | 节点输出 rclcpp 16.0.21 |
| `lock` | 195 个 apt 包，快照 `2026-08-07` / `20260808T004205Z` |
| `verify` | 199 行 ok |
| `deps check` | 全部就绪 |
| 锁定版本是否在快照中 | 195 个里有 **122 个不在**快照中，实测证实发现 2 |
| `uninstall --yes` | 16 秒；`check` 三项回到 `MISSING`；已装包列表与基线**完全一致** |

**第二轮：快照回放。** 锁文件的文件头与源码提交取自第一轮 `lock`，apt 条目换成按快照解析出的 195 个版本（相当于"一台在快照日安装的机器"执行 `lock` 的结果）。

1. **第一次真实 `apply --locked` 失败，发现 6。** apt 报 `NO_PUBKEY AD19BAB3CBF125EA`。keyserver 返回的密钥是 ASCII armor 格式，rospm 原样存成 `.gpg`，apt 不接受。已修复。
2. **重跑仍然失败，发现 7。** 计划第一步是无条件的 `apt-get update`，上次留下的坏快照源让它直接失败，修复后的密钥步骤轮不到执行。未修复，用 `uninstall --yes` 恢复。
3. **`uninstall` 漏删快照 keyring，发现 8。** 一处原因是探测把"armor 格式"当成"不存在"，已拆成两个字段；另一处是 ROS 源文件已删除时，孤儿 keyring 与 Ubuntu 快照源不会被清理。均已修复并实测清除。
4. **保留 `curl` 精确版本时，第 11 步失败，实测证实发现 3。** 前 10 步都成功，包括密钥、两个快照源和 update。安装一步报依赖冲突，一个包都没装。
5. **从锁文件删掉 `curl` 后成功，45 秒。** 新装 212 个包，`python3-rosdep` 从 0.27.0 降级到 0.26.0，`git checkout --detach` 固定到锁定提交，跳过 `rosdep install`。两个快照源在重跑时都识别为"已配置"并跳过。
6. **回放后的环境。** `verify` 198 行全部 ok，退出 0。`deps check` 与 `check --strict` 都就绪。节点编译运行正常，使用锁定的 rclcpp 16.0.19。
7. **重新 `lock` 可复现。** 与输入锁文件逐条比较，194 个 apt 版本完全一致；只多出清单里的 `curl`，即被删掉的那条，记录的是本机版本。
8. **`uninstall --yes` 收尾。** 删除 191 个 `ros-humble-*`、`ros2.list`、快照 keyring 与 `rospm-ubuntu-snapshot.list`；`check` 回到 `MISSING`，apt update 正常。与基线相比只差 `python3-rosdep` 停在 `0.26.0-1`，因为 `uninstall` 按决策 #15 保留共用工具。

**结论。** 在锁定版本确实存在于快照、且锁文件不含比快照新的非 ROS Ubuntu 包这两个前提下，`apply --locked` 在真实主机上可用，并且能精确复现。两个前提正是发现 2 和发现 3，仍需在代码层面解决。

**补充发现：**

6. **已修复：快照 keyring 以 armor 格式写入 `.gpg`。** 现在 `fetch_key` 在指纹核对后，若内容是 armor 格式，就先 `gpg --dearmor` 再安装。探测新增 `snapshot_keyring_armored`：遇到 armor 格式的 keyring，`apply` 会重装，`uninstall` 仍会删除。新增单元测试，以及一个联网的 `#[ignore]` 测试，确认安装进去的是二进制格式。
7. **未修复：失败的 `apply --locked` 会让后续 `apply` 卡住。** 计划起始的 `apt-get update` 在修复坏源之前执行，只能靠 `uninstall` 恢复。建议在前置工具已安装时跳过起始的 update 与前置包安装，或先修复 rospm 自己的源文件再 update。
8. **已修复：`uninstall` 在没有 ROS 源文件时漏删快照残留。** 现在即使 `ros2.list` 已不存在，也会删除快照 keyring 与 `rospm-ubuntu-snapshot.list`；`--keep-source` 时保留。新增单元测试。
9. **未修复：CLI 测试依赖主机状态。** `crates/rospm-cli/tests/apply_locked.rs` 断言计划里出现快照密钥指纹。但主机已有该 keyring 时计划会跳过下载，测试就失败，本机实验期间出现过。建议测试不依赖真实的 `/etc/apt` 与 keyring，或者只在 keyring 不存在时断言。
10. **小问题：rosdep 源已指向同一镜像时仍然改写一次。** 结果不变，但计划里多了一个写文件步骤。

### 2026-09-27 修复

- **发现 2（锁定版本不在快照里）已解决。** `lock` 下载快照 `Packages.gz` 逐包核对：ROS 快照只记录含全部锁定 `ros-<distro>-*` 版本的日期，否则不记录并列出缺失版本；Ubuntu 快照时间戳改为本机 dpkg 最后变更时间；其余包在两个快照都找不到时列出。本机（未装 ROS）真实 `lock`：两个快照的索引都下载核对，4 个包全部命中，约 11 秒。
- **发现 3（顶层 Ubuntu 包精确固定）已解决。** ROS 层与工具包仍精确安装；其余 Ubuntu 包本机版本更新时不降级、计划中列出，`verify` 报 `info`。`apply --locked --dry-run` 实测：锁 `curl …1ubuntu1.25`、本机 `…1ubuntu1.29`，计划不再点名 `curl=`。
- **发现 7 已修复。** 起始 `apt-get update` 前先删除本次会重写的 rospm 自有源文件。
- **发现 9 已修复。** 指纹断言只在主机没有可用快照 keyring 时进行。
- **另修：锁文件缺底包时 `verify` 报就绪。** 现在底包一行 `MISSING`。
- 仍未做：用新规则在干净机器上真实 `apply --locked`（需用户同意实装），以及下文的时间点 2 与 24.04。发现 10 未处理。

### 时间点 2 与 24.04 要看什么

- **ROS 快照是否更新。** 若 humble 出现晚于 `2026-08-07` 的快照，且包含 `ros-humble-ros-base 0.10.0-1jammy.20260908.101203`，第 2 条对"今天从实时源安装"的影响会消失，但问题本身仍在，直到修改 `lock`。
- **旧快照是否仍可用。** 用 `2026-08-07` 快照合成的锁在时间点 2 是否仍能完整解析，这是"间隔数周 `apply --locked`"验收的前提。
- **24.04 / Jazzy。** 在该机器上 `cargo build --release` 后运行同一脚本，汇总表可与本节逐行对比。真实闭环可以照搬上一节的做法；2026-09-27 起不再需要手工合成快照锁、去掉非 ROS 的 Ubuntu 包，直接用 `lock` 的结果，并留意其中的 `snapshot_lacks_versions` / `not_in_snapshots` 说明。
- **真实闭环复测。** 时间点 2 在本机重复上一节两轮流程，确认旧快照仍能回放；若 humble 已有新快照，再试一次"实时安装 → `lock` → 回放"，观察第 2 条是否暂时消失。

## 时间点 2：2026-10-07，Ubuntu 24.04 / Jazzy

使用同一 `scripts/p2-locked-sandbox-check.sh`，工作目录为 `/tmp/rospm-validation-20261007/p2-sandbox/`。

```
time (UTC)                                   2026-10-07T14:14:45Z
host                                         Ubuntu 24.04 noble amd64
rospm lock (network)                         exit 0
ROS snapshot / Ubuntu snapshot               2026-09-11 / 20261007T141443Z
sandbox apt-get update (snapshots)           exit 0
sandbox apt-get update (host+snapshots)      exit 0
locked versions not in snapshot (this lock)  0 / 1
ros-jazzy-ros-base live / snapshot           0.11.0-1noble.20261006.035407 / 0.11.0-1noble.20260903.093452
sim: clean install from snapshot             exit 0, 669 pkgs
sim: locked install, clean machine           exit 0, 669 pkgs
sim: locked install, this host (+live Ubuntu) exit 0, 126 pkgs
```

当前时间点的 Jazzy 快照索引、干净 dpkg 状态解析和当前主机共存源模拟安装均成功；随后已在卸载后的干净状态完成真实 `apply --locked`，见下节。

### 时间点 2 真实快照回放

随后使用 P1 的 `uninstall --yes` 将本机恢复到无 `/opt/ros`、无 ROS apt 源的状态，在 `/tmp/rospm-validation-20261007/p2-sandbox/synth/` 使用上述快照锁执行真实 `rospm apply --locked`。命令成功完成快照 keyring、ROS/Ubuntu 快照源、`apt-get update`、205 个锁定 apt 条目、源码 checkout 和 rosdep cache 更新；`check --strict`、`verify`、`deps check` 均退出 0。最后再次执行 `rospm uninstall --yes`，删除 ROS 包和 rospm 快照源，恢复干净状态。证据日志为 `17-apply-locked-real.log`、`18-locked-check.log`、`19-locked-verify.log`、`20-locked-deps.log`、`21-locked-uninstall.log`。
