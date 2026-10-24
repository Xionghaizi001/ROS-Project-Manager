# Phase 3.5 受管 Conda 验收

`../p35-managed-conda-check.py` 在 Docker 中准备 ROS 主机前置条件，保存不含 Conda 的镜像，再以普通用户执行真实 Conda 安装、环境求解、锁生成与两个独立容器中的锁定重放。宿主机只需 Docker 和对应架构的 rospm 二进制。

```sh
cargo build --locked -p rospm-cli
sudo docker build --platform linux/amd64 -t rospm-p3:amd64 -f scripts/p3/Dockerfile scripts/p3
sudo python3 scripts/p35-managed-conda-check.py \
  --root /tmp/p35-managed-new --rospm target/debug/rospm
```

输出目录必须不存在。脚本保存执行命令、退出状态、原始日志、二进制摘要、fixture、脚本副本、源码提交、锁文件和实际包记录；结束时删除自己创建的容器及临时镜像，保留证据目录。默认在容器内使用清华 Ubuntu / ROS 镜像；`--mirror official` 使用官方源。Conda 安装器仍来自程序内固定的 GitHub URL，包仍来自清单声明的 conda-forge。

要复测 ARM，先按 `scripts/p3-qemu-check.sh` 的说明准备 QEMU / binfmt、ARM 二进制和 `rospm-p3:arm64` 镜像，然后传入 `--platform arm64 --rospm target/aarch64-unknown-linux-gnu/debug/rospm`。脚本的架构参数不会把 x86 验收结果当成 ARM 实测。

每个重放容器只具有 ROS 前置条件、清单、环境定义、两个锁文件及 Git 源码入口，没有 Conda、项目环境、源码检出或编译产物。实际一键入口是：

```sh
rospm --manifest /home/tester/project/rospm.toml apply --mirror tuna --locked --build
```

`build.verify` 自动执行 `ros2 run p35_probe probe`。节点断言解释器与 pyfiglet 都来自受管环境，并创建、销毁真实 rclpy 节点。脚本检查重复执行未重建 Conda 或环境、锁和 shell 启动文件未变、源码提交相同，以及两次重放的完整 Conda 包身份及摘要记录相同。

错误分支包含真实 CLI 拒绝篡改的显式锁、缺少本平台 Conda 锁记录，以及人工构造安装中断后的残留 prefix 并由真实安装器恢复。安装器下载摘要错误、安装命令失败后的重试和两种架构安装器选择还由 `cargo test -p rospm-core conda::` 的确定性测试覆盖；这些模拟测试不等同于在 ARM 硬件上安装或实际断电。
