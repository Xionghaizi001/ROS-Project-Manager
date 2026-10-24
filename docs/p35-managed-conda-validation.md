# Phase 3.5：干净 Conda 环境一键还原验收

日期：2026-10-25。结果：**通过**。两个独立干净 Conda 容器均一键还原、构建、运行节点成功，84 个实际 Conda 包记录逐项一致。Phase 3.5 第二项验收完成；具体真实运行与模拟测试边界如下。

## 环境与入口

Ubuntu 24.04 / ROS Jazzy、x86_64 Docker 容器，以普通用户 `tester` 执行 rospm。先在容器内准备 ROS 主机前置条件，保存不含 Conda、安装器缓存、项目环境、源码检出及编译产物的基线镜像，再创建两个独立容器。宿主机的 ROS、Conda、apt 源及 shell 文件未修改。

输入为项目清单、`environment.yml`、`rospm.lock`、本平台 Conda 显式锁及本地 Git 源码入口；本地 Git 仓库模拟可获取的源码源站，不预先检出到 workspace。每次重放只执行：

```sh
rospm --manifest /home/tester/project/rospm.toml apply --mirror tuna --locked --build
```

该命令自动安装 Miniforge、按显式锁创建项目环境、还原系统依赖和源码、构建 ament_python 包，并通过清单的 `build.verify` 执行 `ros2 run p35_probe probe`。无需手动安装 Conda、激活环境或再次执行 build。节点导入仅受管环境提供的 pyfiglet，检查 Python 与依赖路径，创建、销毁真实 rclpy 节点。

Miniforge 固定为 **26.7.2-0**，x86_64 安装器来自内置 GitHub URL，SHA256 为 `281b0ac7d550802efc81af633225a5e6116d29ae72f3ab4eae7168c3931a4c05`。Ubuntu / ROS 使用清华镜像准备前置条件；Conda 包来自 conda-forge。项目环境为 Python **3.12.15**、numpy **1.26.4**、pyfiglet **1.0.4**、colcon-core **0.21.3**，共 **84 个 Conda 包、0 个 PyPI pin**。锁记录另包含 **204 个 apt 包和 1 个源码提交**。锁定系统依赖使用真实验签的 ROS **2026-10-05** 与 Ubuntu **20261024T174225Z** 快照。

## 覆盖方式

| 验收项 | 方法 |
| --- | --- |
| 全新环境一键还原、构建与节点运行 | 两个独立容器，各自重新下载、校验并安装 Miniforge；`apply --locked --build` 自动运行验证命令 |
| 第二环境包记录一致 | 比较全部 Conda 元数据的 name、version、build、subdir、sha256、md5、url；锁文件和源码提交另行核对 |
| 重复执行复用 | 再次执行同一命令并运行节点；比较 Conda 可执行文件与环境 history 的 inode、mtime，确认未重建 |
| 安装中断后重试 | 在第二容器人工放置无完成标记的安装 prefix 和残留文件，真实安装器重装成功，残留消失；核心测试另覆盖安装命令实际返回失败后的重试与离线缓存复用 |
| 摘要错误 | 真实 CLI 拒绝修改过的 Conda 显式锁；核心测试注入错误安装器下载内容，断言未执行安装器、清除临时文件，并覆盖坏缓存重新下载 |
| 缺少锁记录 | 删除本平台 Conda 锁记录，真实 CLI 退出非零，明确拒绝重新求解；恢复锁后继续成功还原 |
| x86_64 / aarch64 安装器选择 | 核心测试检查两平台内置 URL、架构后缀、版本和摘要格式；另在线核对两份官方 `.sha256` 资产与内置摘要一致；x86_64 另有真实下载、摘要校验与执行 |
| shell 启动文件不变 | 基线与重放、重复执行后比较 `.bashrc`、`.profile` SHA256 |

本次真实端到端范围是 **x86_64 容器**；安装器错误注入与 aarch64 选择是确定性测试，不冒充真实断电或原生 ARM 安装验收。ARM 原生发布门槛仍见 Phase 3。脚本支持指定 ARM 平台与对应二进制，供后续复跑。

## 复跑与证据

可复跑脚本：[p35-managed-conda-check.py](../scripts/p35-managed-conda-check.py)；fixture 与准备步骤：[scripts/p35](../scripts/p35/README.md)。每次使用新的输出目录，脚本只删除自己创建的容器和临时镜像，保留证据。

本次证据目录：`/tmp/rospm-p35-managed-20261025-v4`。包含 fixture、执行时脚本、二进制摘要、每步命令与原始日志、源码提交、两个锁文件、两份实际包记录和 `results.json`。`seed-apply-build.log` 记录首次自动安装及构建；`replay-*-one-command.log` 记录一键重放；`repeat-*.log` 记录复用；`reject-*.log` 记录拒绝非法输入；`retry-discarded-partial-prefix.log` 记录安装残留恢复。

首次准备使用官方 Ubuntu 源下载停滞，改为仅容器内的清华源；后续复跑复用只含 ROS 前置条件的镜像 `rospm-p35:ros-ready`，不复用 Conda、环境、缓存或构建产物。旧失败/中止尝试日志保留在同名前缀的目录，不计入通过结果。

代码回归：`cargo test --locked --workspace` **249 通过、0 失败、8 跳过**（原有需外部环境的测试）；`cargo fmt --check`、`cargo clippy --locked --workspace --all-targets -- -D warnings` 通过。相关确定性测试位于 `crates/rospm-core/src/conda/tests.rs` 与 `conda/bootstrap.rs`，包括安装器选择、错误摘要拒绝、中断重试、缺锁及精确重放。

最终实际包记录文件 SHA256：`fba7885d8cc40a92945b62196291aaeb072767e7675f3ec091847186f0d49597`（两份一致）。最终 `result.txt` 为 PASS；逐步命令结果见 `results.json`。
