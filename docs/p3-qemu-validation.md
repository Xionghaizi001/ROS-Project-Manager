# Phase 3 QEMU aarch64 集成验收

开发阶段使用 x86_64 主机上的 Docker + QEMU user-mode，正式发布前再用原生 ARM runner 复测。QEMU 共享主机内核，本项验证 ARM64 用户态的 native 提供者、apt、编译与节点运行；不验证 Raspberry Pi / Jetson 的内核、GPU、驱动和硬件接口。

## 搭建与重跑

Ubuntu x86_64 主机需要 Docker、QEMU binfmt 和 ARM64 Rust 交叉编译器。首次安装：

```bash
sudo apt-get update
sudo apt-get install -y docker.io qemu-user-static binfmt-support gcc-aarch64-linux-gnu
sudo systemctl start docker
sudo update-binfmts --enable qemu-aarch64
rustup target add aarch64-unknown-linux-gnu
sudo docker run --rm --platform linux/arm64 ubuntu:24.04 uname -m
# 应输出 aarch64
```

在仓库根目录执行（需联网，下载 Ubuntu / ROS 包，耗时取决于 QEMU 和网络）：

```bash
DOCKER='sudo docker' scripts/p3-qemu-check.sh
# 或指定一个新的结果目录和实际存在的 ROS 快照
ROS_SNAPSHOT=2026-10-05 DOCKER='sudo docker' \
  scripts/p3-qemu-check.sh .tools/p3-qemu-new-run
```

容器使用结果目录中固定的脚本和 fixture 副本，仓库后续编辑不会改变正在执行的容器测试。脚本在主机交叉编译 ARM64 rospm，避免在仿真环境内编译 Rust。Dockerfile 仅准备系统工具，不含 ROS。安装和源码还原均由隔离容器中的 root 用户通过 native 提供者执行（保留产品实际调用的 sudo 命令）；主机不安装 ROS、不改主机 apt 源。默认 QEMU binfmt 的 `PF` 标志不能传递模拟 setuid 程序的凭据，因此使用 root 容器完成 apt 安装，不修改主机的 binfmt 凭据策略。容器退出时自动删除，Docker 测试镜像和结果目录保留供复查；需要重跑时使用新目录。

`ROS_SNAPSHOT` 未指定时从官方索引发现最新日期。采集环境先配置经完整指纹校验的签名快照，避免“实时 ROS 版本尚未进入快照”导致锁降级；回放容器没有这个预置源，须由 `apply --locked` 自行建立快照配置。清单启用真实 rosdep，源码来自本次创建的本地 git 仓库，只有一个依赖 rclcpp 的 C++ 节点。

若 ARM 采集已经成功生成 `aarch64.lock`，而后续步骤中断，可以在新结果目录恢复：

```bash
P3_ARM_SEED="$PWD/.tools/p3-qemu-previous" DOCKER='sudo docker' \
  scripts/p3-qemu-check.sh .tools/p3-qemu-resumed
```

恢复时校验两架构 rospm 二进制完全相同、fixture 与源提交不变、清单哈希一致，并复制原 ARM 采集日志。仅复用真实采集的锁记录；x86 最终锁仍重新生成，ARM 锁定回放仍使用全新容器。

## 验收流程

1. x86_64 Ubuntu 24.04：真实 `apply`、`check --strict`、`check --target x86_64 --strict --json`、`deps check`、`lock`，记录 x86 包版本。
2. QEMU ARM64 Ubuntu 24.04：先确认仅含 x86 条目的锁被 `apply --locked --dry-run` 明确拒绝；再真实安装并 `lock`，自动保留 x86 条目、补全 ARM 条目。验证内置映射把 `binutils-x86-64-linux-gnu` 替换为 `binutils-aarch64-linux-gnu`。
3. 把合并锁传回 x86，真实执行 `lock`，断言两平台底包、工具和替换包均有记录，两个快照均非空，输出最终多平台锁。
4. 新建无 ROS、无源码 checkout 的 ARM64 容器，传入同一清单与 x86 最终生成的锁，真实 `apply --locked`，断言系统中的两个快照源与输入锁完全对应且 ROS 源为 arm64；随后 `check`、目标仓库检查、`verify`、`deps check`、`build`、`ros2 run`，均须成功。校验节点 ELF 为 AArch64、输出架构和节点名正确、git HEAD 与锁定提交一致、输入锁未被更改。
5. 使用明确不存在的目标包验证严格检查返回非零且 JSON 为 `missing`。恢复清单，重复锁定还原与核验，最后重新 `lock`，比较两平台包版本、源码记录及清单哈希完全相同。

`lock` 当前只能解析本机已安装包；`check --target` 查询包名可用性，不解析目标架构的精确版本。因而仅在 x86 上执行一次 `lock` 不会凭空生成 ARM 锁记录：必须先在 ARM 环境采集并合并，再由 x86 保存最终锁。本流程验证的是这条真实支持的多平台工作流，不将 x86 包版本直接改写成 ARM 包版本。

脚本使用 `set -euo pipefail`；镜像构建、每条 CLI 命令、锁比较均记录日志，任何失败都不会写入 `result.txt` 的通过标记。结果在 `.tools/p3-qemu-<UTC 时间>/`，其中 `environment.txt` 记录版本、镜像 ID、源码基线与二进制摘要，`multiplatform.lock` 是回放输入，`replayed.lock` 是回放后重新采集的锁，`p3-native-aarch64` 保留实际运行的节点 ELF，`suite.log` 记录步骤，`*-*.log` 记录完整命令输出。

## 2026-10-20 实测（UTC+8）

主机为 Ubuntu 22.04 x86_64 / WSL2，Docker 29.1.3、QEMU 6.2.0（Ubuntu `1:6.2+dfsg-2ubuntu6.31`）、Rust 1.98.1。两个目标环境均为 Ubuntu 24.04 / ROS Jazzy，产品代码基线为 `63d804b21613a51d3fffa567cfdb2e5260761184`，使用本次新增的测试脚本与 fixture。

ARM 首次真实安装、仓库检查、依赖检查和 `lock` 已采集成功。本次后续回放通过 `P3_ARM_SEED` 复用这份记录，经过二进制、fixture、源码提交和清单哈希校验；原始 ARM 采集日志复制在最终结果目录的 `seed-arm-evidence/`。x86 最终生成锁及 ARM 新容器回放均重新实际执行。

- 最终证据目录：`.tools/p3-qemu-20261020-final/`。
- x86 最终锁：`multiplatform.lock`，共 410 条 apt 记录，两平台各 205 条；ROS 快照 `2026-10-05`，Ubuntu 快照 `20261019T172834Z`，签名核验通过。
- 锁定源码提交：`02a5d490122cf6fdd9def36148ee41303b593285`；清单 SHA256：`5e3d1adcb879fdb77a5d85e8a6815bbd0398ed0eaa6fc6dde3220aad6cb45cb5`。

| 验收项 | 实测结果 |
| --- | --- |
| x86 对照环境 | `apply`、严格检查、目标仓库查询、`deps check`、`lock`、`build`、`ros2 run` 均成功 |
| ARM 首次采集 | 真实安装及所有检查成功，生成 205 条 ARM 记录，保留 205 条 x86 记录 |
| x86 最终生成多平台锁 | 成功；两平台底包、工具和目标替换包均有记录，两个快照均非空 |
| 新 ARM 容器 `apply --locked` | 成功；自动下载密钥、配置并使用两个快照、安装锁定版本、导入源码及检出锁定提交 |
| ARM `check --strict` / 目标仓库查询 / `verify` / `deps check` | 均退出 0；两个系统快照源与输入锁完全对应，ROS 源声明 `arch=arm64` |
| ARM `build` | 退出 0；colcon 报告 1 个包完成，总计约 2 分 1 秒 |
| ARM `ros2 run p3_native probe` | 退出 0，输出 `P3_NATIVE_OK arch=aarch64 node=p3_native_probe`；保存的节点为 ELF64 / AArch64 |
| 内置替换 | ARM 安装与锁记录使用 `binutils-aarch64-linux-gnu=2.42-4ubuntu2.10`；ARM 记录不含 x86 的工具链包名 |
| 缺少 ARM 锁记录 | `apply --locked --dry-run` 非零退出，明确报告缺少 `linux-aarch64` 的锁定 apt 包，ROS 保持未安装状态 |
| 目标包不存在 | 严格查询非零退出，JSON 对 `rospm-p3-deliberately-missing-package` 明确返回 `missing` |
| 重复还原、重新采集的一致性 | 成功；三批 apt 操作均为零变更，再次 `verify` 通过，输入锁 SHA256 不变；重新 `lock` 保持两平台各 205 条包版本、源码记录及清单哈希一致 |

两平台的包版本由各自系统真实采集。例如 rclcpp 均为上游 `28.1.22`，但 x86 二进制版本是 `28.1.22-1noble.20260911.105434`，ARM 是 `28.1.22-1noble.20260911.060156`，因此必须分别记录。

重新采集的 `replayed.lock` 与输入相比，仅生成时间及 Ubuntu 快照时间戳变化；Ubuntu 快照为 `20261019T180633Z`，同样验签通过。`lock` 按当前 dpkg 状态文件时间重新选择快照，因此此处比较包版本、源码和清单哈希的一致性，不要求重新生成的锁逐字节相同；两次 `apply --locked` 均未修改输入锁。

回放输入锁文件 SHA256 为 `3be4da7be90c5e852befbe8ed515661d94de96c2cae6ee52bbf82619c2d7182e`；实际运行的 ARM 节点 SHA256 为 `a2b66fa563fcaf20d349b50b60c1f942913335d357ecaedb98470fef29ad2193`。主机还保留 `rospm-p3:locked-arm64-checkpoint` 镜像作为安装后环境的本地检查点，验收的起点仍是无 ROS 的基础镜像。

Rust 回归：`cargo test --workspace --offline` 共 168 项通过、0 失败，5 项既有联网测试按配置忽略；`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --offline -- -D warnings` 均通过。Bash 语法检查和 Python 语法解析通过。该版本的 QEMU 中 Python 字节码编译与 rosdep YAML 解析耗时较长，完整首次采集和回放应预留充足时间。

## 正式发布前

仍须在 GitHub `ubuntu-24.04-arm` 原生 runner 上以普通用户执行同一 fixture 的锁定还原、编译与运行，覆盖真实 sudo 提权并保存日志；真实 Raspberry Pi / Jetson 的硬件适配另行验收。当前开发测试不依赖 runner 可用性。
