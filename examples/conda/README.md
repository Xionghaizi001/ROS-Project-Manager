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
# lock_file = "conda-{platform}.lock"   # 默认值；`rospm lock` 写入，`apply --locked` 重放
```

如果需要额外依赖，可在此目录创建 `environment.yml`，并启用 `environment_file`：

```yaml
dependencies:
  - colcon-common-extensions
  - numpy
```

`apply --dry-run` 展示规范化定义与创建命令；`apply` 自动准备 `<项目目录>/.rospm/conda/env`，随后继续原有系统依赖与源码还原；`build` 使用该 prefix，不会自行创建环境。同名项目位于不同目录时不会共用环境。`PATH` 中没有 24.3 或更新版本的 Conda 时，`apply` 自动下载固定版本的 Miniforge，校验 SHA256 后安装到 `~/.local/share/rospm/conda/`（无需 sudo，不修改 shell 启动文件），再继续还原；`build` 随后使用同一个 Conda。

外部已有环境缺失时会明确报错，rospm 不会修改它。受管环境创建完成后才写入完成记录；定义相同的完整环境可以复用，已有目录与记录不匹配时会报错并要求先移走目录，自动恢复与更新留待后续任务。Python 版本和 channels 可声明在清单中，环境文件内若有冲突则报错；文件中的名称和 prefix 不决定受管环境位置。

`apply` 之后执行 `rospm lock`，会把受管环境写成本平台的显式锁 `conda-linux-64.lock`（`conda list --explicit --sha256`），并在 `rospm.lock` 中记录其摘要与环境定义摘要；两个文件一起提交。另一台机器上 `rospm apply --locked` 用 `conda create --file` 按锁文件精确安装，不重新求解；缺少本平台记录、定义已变化或摘要不符时直接报错。`environment.yml` 中的 pip 依赖须全部写成 `name==version`（包括传递依赖），重放时以 `--no-deps` 安装。

外部已有环境（`name` / `prefix`）只检查存在，不记录锁，也没有可重放保证。完成记录与锁都不保证 apt ROS 与 Conda Python 的 ABI 兼容；构建前兼容性诊断、`env` / `shell` 对接属于 Phase 3.5 后续任务。
