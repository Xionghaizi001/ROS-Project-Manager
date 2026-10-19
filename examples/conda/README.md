# Conda 构建环境选择

此清单以 native ROS Jazzy 为基础，为所有 workspace 选择已有的 `robot-dev` Conda 环境。将 ROS 源码放到 `ws/src`，确保 Conda 可从 `PATH` 调用，且环境中已有兼容的 Python 与 colcon。

从仓库根目录预览（不要求已安装 Conda，不写文件）：

```sh
rospm --manifest examples/conda/rospm.toml build --dry-run
rospm --manifest examples/conda/rospm.toml build --conda-env other --dry-run
rospm --manifest examples/conda/rospm.toml build --conda-prefix ./my-env --dry-run
```

CLI 选项互斥，并覆盖清单中的整个选择，不写回清单。CLI 相对路径从当前目录解析；清单中的 `prefix` 从清单所在目录解析。去掉 `--dry-run` 执行构建。

当前只选择已有环境；名称或路径不锁定包版本，也不保证 apt ROS 与 Conda Python 的 ABI 兼容。自动安装与创建环境、构建前兼容性诊断、`env` / `shell` 对接属于 Phase 3.5 后续任务。
