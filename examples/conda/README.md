# Conda 构建环境选择

此清单以 native ROS Jazzy 为基础，为所有 workspace 选择已有的 `robot-dev` Conda 环境。将 ROS 源码放到 `ws/src`，确保 Conda 可从 `PATH` 调用，且环境中已有兼容的 Python 与 colcon。

从仓库根目录预览（不要求已安装 Conda，不写文件）：

```sh
rospm --manifest examples/conda/rospm.toml build --dry-run
rospm --manifest examples/conda/rospm.toml build --conda-env other --dry-run
rospm --manifest examples/conda/rospm.toml build --conda-prefix ./my-env --dry-run
```

CLI 选项互斥，并覆盖清单中的整个选择，不写回清单。CLI 相对路径从当前目录解析；清单中的 `prefix` 从清单所在目录解析。去掉 `--dry-run` 执行构建。

要改用 rospm 管理的环境，删除 `[build.conda]` 下的 `name` / `prefix`，替换为：

```toml
[build.conda.managed]
python = "3.12"
channels = ["conda-forge"]
# environment_file = "environment.yml"  # 可选，路径以清单目录为基准
# lock_file = "conda.lock"              # 仅定义引用；配置后 apply 暂时明确拒绝
```

如果需要额外依赖，可在此目录创建 `environment.yml`，并启用 `environment_file`：

```yaml
dependencies:
  - colcon-common-extensions
  - numpy
```

`apply --dry-run` 展示规范化定义与创建命令；`apply` 自动准备 `<项目目录>/.rospm/conda/env`，随后继续原有系统依赖与源码还原；`build` 使用该 prefix，不会自行创建环境。同名项目位于不同目录时不会共用环境。Conda 须已在 `PATH` 中，自动安装由下一项任务实现。

外部已有环境缺失时会明确报错，rospm 不会修改它。受管环境创建完成后才写入完成记录；定义相同的完整环境可以复用，已有目录与记录不匹配时会报错并要求先移走目录，自动恢复与更新留待后续任务。Python 版本和 channels 可声明在清单中，环境文件内若有冲突则报错；文件中的名称和 prefix 不决定受管环境位置。

名称、路径及完成记录不锁定包版本，也不保证 apt ROS 与 Conda Python 的 ABI 兼容。精确锁定重放、构建前兼容性诊断、`env` / `shell` 对接属于 Phase 3.5 后续任务。
