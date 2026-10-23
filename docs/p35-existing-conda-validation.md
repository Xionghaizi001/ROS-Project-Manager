# Phase 3.5 第一项：已有 Conda 环境真实验收

日期：2026-10-22。结果：**通过**。范围仅为 `phases.md` Phase 3.5 第一项；不代表第二项“未安装 Conda 的干净机一键还原”通过。

## 环境清理与准备

在用户确认可清理的 Ubuntu 22.04.5 x86_64 虚拟实例执行，全程未操作 `/mnt/*`。

- 用 `rospm uninstall --yes` 移除原有 191 个 `ros-humble-*` 包及 ROS apt 源、密钥，确认 `/opt/ros/humble` 不存在。保留共享的 rosdep、vcstool、colcon 与项目源码。检查未发现真实 Conda 安装，`/tmp` 中旧 CLI 单测的模拟环境不算 Conda 安装。
- 卸载后的 Ubuntu 官方源索引刷新反复超时，终止该次 `apt-get update`。随后使用 `/tmp/rospm-p35-validation/sources.list` 临时源（清华 Ubuntu / ROS 镜像）重新安装 `ros-humble-ros-base` 与构建工具，未修改系统 Ubuntu 源。ROS 密钥指纹核对为 `C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654`。
- 安装 Miniforge **26.7.2-1**（Conda 26.7.2）到 `/tmp/rospm-p35-validation/miniforge`，使用批处理 `-b`，不执行 `conda init`。安装器来自清华镜像，其 SHA256 与 GitHub 官方该版本摘要一致：`cf0df84220e1c83a1cb2f447056594ca458fa4c6a353430c498f281ec21bd187`。这是手动准备已有环境，不使用 rospm 自动安装器固定版本表。
- 创建 `p35-py310`（Python **3.10.22**），安装 `colcon-common-extensions`、`catkin_pkg`、`empy==3.3.4`、`setuptools<80`、`numpy<2`、`pyfiglet`，并克隆到独立 prefix `env-other`。不兼容环境使用 Miniforge base 的 Python **3.14.8**，也安装 colcon、catkin_pkg、empy，确保报错确实来自 ABI 检查。
- ROS 底包 `0.10.0-1jammy.20260915.225919`，rclpy `3.3.22-1jammy.20260907.213431`。Conda numpy 1.26.4、pyfiglet 1.0.4、colcon-core 0.21.3。

环境及完整日志保留在 `/tmp/rospm-p35-validation`；本次结束时 ROS 与两个兼容 Conda 环境均保留。系统 ROS 源已被卸载流程移除，重装使用上述临时源；后续 apt 管理 ROS 包需显式使用临时源或重新配置 ROS 源。

## 方法与结果

可重复执行脚本：[p35-existing-conda-check.py](../scripts/p35-existing-conda-check.py)。脚本每次要求全新的输出目录，不删除已有工作区。子进程清除 Conda 激活及 ROS 相关变量；PATH 仅额外暴露 `conda` 可执行文件，不加入 Conda Python；Bash 使用 `--noprofile --norc`。因此调用端是未激活环境。

每个工作区包含一个 `ament_python` 包和一个 `ament_cmake` 包。前者用 `ros2 run` 创建并销毁真实 rclpy 节点；后者生成真实 Python 解释器 CMake 缓存用于切换检查。

| 检查 | 结果 |
| --- | --- |
| 系统 Python 无目标依赖 | `/usr/bin/python3` 确认不能找到 `pyfiglet` |
| `[build.conda].name` | 构建两个包并运行节点成功，解释器与 `pyfiglet.__file__` 均属于 `p35-py310` |
| `[build.conda].prefix` | 独立环境 `env-other` 构建并运行节点成功，解释器及依赖均属于该 prefix；项目路径含空格 |
| 两种构建预览 | 对完整项目目录树与文件内容摘要做前后比较，`build --dry-run` 零写入 |
| 缺失环境名称与路径 | 均退出 1，明确 `Existing Conda environment … was not found`，未创建构建目录 |
| ABI 不兼容 | Python 3.14 环境退出 1，明确 ROS 使用 Python 3.10、rclpy 扩展无法加载，建议使用 Python 3.10，未开始构建 |
| 切换后的缓存 | 原 name 构建后以 `--conda-prefix env-other` 重建，退出 1，列出真实 `p35_cache` 目录和旧解释器，建议清理或分开 build/install；缓存字节保持不变 |
| native 回归 | 不声明 Conda，两个包构建成功，节点实际解释器 `/usr/bin/python3` |

最终脚本 **13 项断言全部通过**（包含系统依赖缺失、两个预览、三次构建、三次节点运行、两次缺环境、ABI、缓存）。另执行 `cargo test -p rospm-cli --test build`，**8/8 通过**。未修改产品实现。

节点输出摘录：

```text
EXTRA=/tmp/rospm-p35-validation/miniforge/envs/p35-py310/lib/python3.10/site-packages/pyfiglet/__init__.py
P35_NODE_OK PYTHON=/tmp/rospm-p35-validation/miniforge/envs/p35-py310/bin/python
EXTRA=/tmp/rospm-p35-validation/env-other/lib/python3.10/site-packages/pyfiglet/__init__.py
P35_NODE_OK PYTHON=/tmp/rospm-p35-validation/env-other/bin/python
P35_NODE_OK PYTHON=/usr/bin/python3
```

准备阶段发现两项限制：首次节点运行因 Conda 缺少 numpy 失败，补齐后通过；构建前检查 rclpy 扩展不代表所有节点运行依赖均齐全。Conda 26.7 自身拒绝创建含空格的环境 prefix，因此本次验证的是普通 prefix + 含空格项目目录，不声称含空格环境 prefix 已通过。

## 复跑

保留上述环境时，从仓库根目录运行（`--root` 必须不存在）：

```bash
python3 scripts/p35-existing-conda-check.py \
  --root /tmp/rospm-p35-validation/recheck \
  --conda /tmp/rospm-p35-validation/miniforge/bin/conda \
  --name p35-py310 \
  --prefix /tmp/rospm-p35-validation/miniforge/envs/p35-py310 \
  --other-prefix /tmp/rospm-p35-validation/env-other \
  --bad-prefix /tmp/rospm-p35-validation/miniforge
```

最终证据：`/tmp/rospm-p35-validation/final/evidence/results.json` 及同目录逐项日志；总日志 `final.log`。卸载前包清单 `packages-before.tsv`，卸载 `uninstall.log`，重装 `apt-update.log` / `apt-install.log`，Conda 准备 `conda-*.log` / `pip*.log`，CLI 回归 `build-regression.log`，均在同一验收根目录。
