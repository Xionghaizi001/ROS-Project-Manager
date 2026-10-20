# rospm 规格草案 v0

> 状态：草案。对应 Roadmap P0。schema 在 P7 发布 1.0 前允许不兼容变更。
> 决策依据见 `decisions.md`。

## 1. 文件与目录约定

```
<project>/
  rospm.toml          # 清单（意图）
  rospm.lock          # 锁文件（结果），由 `rospm lock` 生成，提交到版本库
  *.repos             # vcstool 源码清单，被 rospm.toml 引用
  pixi.toml           # 仅 provider = "pixi" 时存在，被 rospm.toml 引用
  pixi.lock           # 同上，由 pixi 生成
  .rospm/             # 工具缓存与生成物（不提交）
    plan/             # 最近一次 --dry-run / apply 的计划
    env.sh            # `rospm env` 生成的环境脚本
    docker/           # 生成的 Dockerfile 等
```

## 2. 清单 `rospm.toml`

### 2.1 完整示例

```toml
schema_version = 1

[project]
name = "my_robot"
description = "示例项目"
ros_distro = "jazzy"                       # humble | jazzy | rolling
platforms = ["linux-x86_64", "linux-aarch64"]

# ---------- 环境提供者（三选一，只能保留与 provider 对应的子表） ----------
[environment]
provider = "native"                        # native | pixi | docker

[environment.native]
ubuntu = "24.04"
ros_base = "ros-jazzy-ros-base"            # 底包：ros-base（精简）或 desktop（含 RViz / demos）；`rospm init --base` 可选

# provider = "pixi" 时改为：
# [environment.pixi]
# manifest = "pixi.toml"                   # 仅引用；pixi.lock 与之同目录

# provider = "docker" 时改为：
# [environment.docker]
# base_image = "ros:jazzy-ros-base"
# dockerfile = "docker/Dockerfile"         # 省略则由工具生成到 .rospm/docker/
# platforms = ["linux/amd64", "linux/arm64"] # buildx

# ---------- 可选的项目级 Conda 构建环境（native） ----------
# [build.conda]
# name = "robot-dev"                       # 已有环境名称；与 prefix 二选一
# prefix = ".conda/robot-dev"               # 或已有环境路径，相对于清单目录
# 或改用受管环境，不能与上面的 name / prefix 共存：
# [build.conda.managed]
# python = "3.12"
# channels = ["conda-forge"]
# environment_file = "config/environment.yml" # 可选，Conda 环境定义
# lock_file = "locks/conda-{platform}.lock"   # 可选，显式锁路径；默认 conda-{platform}.lock

# ---------- workspace 与叠加顺序 ----------
[workspaces.deps]
path = "deps_ws"
sources = "deps.repos"                     # vcstool 格式
underlays = []                             # 空 = 只叠在 ROS 底包之上

[workspaces.deps.colcon]
defaults = "colcon/deps.defaults.yaml"
mixins = ["release"]
build_args = ["--symlink-install"]

[workspaces.main]
path = "ws"
sources = "main.repos"
underlays = ["deps"]                       # 编译顺序由此推导

[workspaces.main.colcon]
mixins = ["release", "ccache"]
build_args = ["--symlink-install", "--event-handlers", "console_direct+"]

# ---------- 依赖 ----------
[dependencies]
apt = ["libfoo-dev"]                       # package.xml 未覆盖的额外 apt 包
pip = ["numpy>=1.26"]                      # 只作记录（决策 #6）

[dependencies.rosdep]
enabled = true
skip_keys = ["libopencv-dev"]
extra_sources = []                         # 额外 rosdep yaml 源

# ---------- 平台覆盖（决策：替换规则人工声明） ----------
[target.linux-aarch64.dependencies]
apt = ["libbar-arm-dev"]                   # 追加

[target.linux-aarch64.replace]
"nvidia-cuda-toolkit" = "cuda-toolkit-12-6"

[target.linux-aarch64.exclude]
apt = ["intel-mkl"]

# ---------- 环境变量 ----------
[env]
ROS_DOMAIN_ID = "42"
RMW_IMPLEMENTATION = "rmw_cyclonedds_cpp"
ROS_LOCALHOST_ONLY = "0"

# ---------- 编辑器 ----------
[editor]
vscode = true
clion = false
devcontainer = true                        # 复用 docker provider 的产物

# ---------- 导出 ----------
[export]
compression = "zstd"                       # none | gzip | zstd
sign = false
```

### 2.2 字段规则

| 表 | 必填 | 规则 |
|---|---|---|
| `schema_version` | 是 | 整数，当前为 1 |
| `project.ros_distro` | 是 | 枚举，见决策 #5 |
| `project.platforms` | 是 | 至少一项；`lock` 按每个平台分别解析 |
| `environment.provider` | 是 | 只允许存在对应的 `environment.<provider>` 子表，多余子表报错 |
| `build.conda` | 否 | 当前仅用于 native；表存在时必须且只能设置 `name`（已有环境名称）、`prefix`（已有环境路径）或 `managed` 子表；空值、未知字段报错；省略此表保持原构建方式 |
| `build.conda.managed` | 否 | 必填 `python`（数字 `major.minor[.patch]`）和非空 `channels` 列表；可选 `environment_file` / `lock_file` 路径，以清单目录为基准；`lock_file` 中的 `{platform}` 展开为 Conda 平台名（`linux-64` / `linux-aarch64`），默认 `conda-{platform}.lock`；`project.platforms` 多于一项时显式的 `lock_file` 必须含 `{platform}`；不允许自定义名称或 prefix |
| `workspaces.<name>` | 至少一个 | `path` 相对于清单所在目录；`sources` 可省略（纯本地源码） |
| `workspaces.<name>.underlays` | 否 | 引用的 workspace 必须存在；整体必须是 DAG |
| `dependencies.pip` | 否 | 只记录，`apply` 不执行 |
| `target.<platform>.*` | 否 | `<platform>` 必须在 `project.platforms` 内；合并顺序：追加 → 替换（用户规则优先于内置映射，同名覆盖可禁用默认项，只替换一次）→ 排除（匹配替换后包名）→ 按顺序去重；内置表与更新方式见 [替换映射表](replacements.md) |
| `env.*` | 否 | 字符串；工具保留 `AMENT_PREFIX_PATH`、`COLCON_PREFIX_PATH` 由叠加顺序自动生成，用户声明时报错 |

`build.conda` 是所有 workspace 共用的默认选择。`name` 不能包含空白、`/`、`\`、`:`、`#` 或 NUL，不能以 `-` 开头，也不能为 `.` 或 `..`；`prefix` 允许空格，但不能是空白字符串或包含 NUL。清单中的相对 `prefix` 从清单所在目录解析；CLI 的相对 `--conda-prefix` 从调用时的当前目录解析；绝对路径直接使用。不展开 `~` 或环境变量，也不要求预览时路径已存在。

`build --conda-env <name>` 与 `build --conda-prefix <path>` 互斥，任一参数均替换清单中的整个 Conda 选择（包括受管环境选择），只影响本次构建，不写回清单；清单仍须先通过结构校验。构建通过 `conda run --no-capture-output -n <name>` / `-p <path>` 启动受控 Bash，再加载 ROS 与 underlay 并执行 colcon；`--dry-run` 展示该完整命令且不执行、不创建文件。未声明 Conda 且未传覆盖参数时继续直接运行原有 colcon 命令。

已有环境模式下，`apply` / `build` 在执行系统安装或编译之前查询 Conda 并检查环境是否存在，缺失时明确报错；不会创建、更新或接管外部环境。名称或路径本身不锁定包版本：`lock` 不为外部环境写锁记录并说明，`apply --locked` 对它仍只检查存在并在计划中说明没有可重放保证。

受管模式下，环境固定为 `<清单目录>/.rospm/conda/env`，不使用 Conda 的全局同名环境；两个同名项目位于不同目录时拥有不同 prefix。`apply` 在系统安装步骤之前准备环境；`build` 只检查环境和完成记录，缺失或定义变化时提示先执行 `apply`。环境创建命令为 `conda env create --yes --prefix <项目 prefix> --file <生成的定义>`，工作目录为清单目录，不执行 `conda init` 或修改 shell 启动文件。

受管模式所用的 Conda 按以下顺序确定（自动引导）：

1. rospm 此前已安装并标记就绪的 Miniforge（`$XDG_DATA_HOME/rospm/conda/miniforge3-<版本>/bin/conda`，默认 `~/.local/share/rospm/...`）。规划阶段只读文件即可确定；此后 `apply` 与 `build` 都使用它，保证创建与运行环境的是同一个 Conda。
2. 否则使用 `PATH` 中的 `conda`，执行时以 `conda --version` 检查，需不低于内置表的 `minimum_conda`（当前 24.3，`conda env create --yes` 所需）。`conda init` 生成的 shell 函数不可见，只认可执行文件。
3. 都不可用时，`apply` 自动安装固定版本的 Miniforge 后在同一次还原中继续；`build` 不安装，报错并提示先执行 `apply`。

安装器版本、下载地址和 SHA256 只来自随 rospm 编译的 [`crates/rospm-core/data/miniforge.toml`](../crates/rospm-core/data/miniforge.toml)（当前 Miniforge `26.7.2-0`，conda-forge 官方 GitHub Release，x86_64 / aarch64 各一项），运行时不从下载源读取摘要；按本机平台选择安装器，表中没有的平台只能使用 `PATH` 中的 Conda。安装器下载到 `$XDG_CACHE_HOME/rospm/downloads/`（默认 `~/.cache/rospm/downloads/`），先写 `.part`，摘要一致才改名为缓存文件；摘要不符时删除下载文件并拒绝执行。已缓存且摘要一致的安装器直接复用，因此离线时只要缓存存在即可安装；摘要不符的缓存视为损坏，删除后重新下载。下载有连接 / 响应超时并对网络错误及 5xx 重试，失败时报告 URL 和缓存路径。

安装以当前用户执行 `bash --noprofile --norc <安装器> -b -p <安装目录>`：`-b` 为批处理模式，不提示、不修改 `.bashrc` 等 shell 启动文件；rospm 不执行 `conda init`，不需要 sudo。安装后 `<安装目录>/bin/conda --version` 满足最低版本才写入同级的 `miniforge3-<版本>.ready`（内容为安装器摘要）。没有该记录的安装目录视为中断的 rospm 安装，下次 `apply` 删除后重装；该目录专属 rospm，不涉及用户自行安装的 Conda。同一份 Miniforge 由该用户的所有项目共用，`uninstall` 不删除它。Conda 自身仍会写入 `~/.conda/environments.txt` 等运行记录。`--dry-run` 展示上述选择顺序、安装器版本、URL、摘要、缓存与安装位置，不下载、不写文件。已有环境模式（`name` / `prefix`）不自动安装 Conda。

`environment_file` 是可选的 Conda YAML；首期接受 `name`、`prefix`、`channels`、`dependencies` 和 `variables`，未知字段报错。文件中的 `name` / `prefix` 不决定受管环境位置，生成定义时会移除；`channels` 可省略，若声明则必须与清单顺序一致（忽略 `nodefaults`）。依赖中的 `python` 可省略或无版本，若指定版本则须与清单的 `python=<版本>` 完全一致，冲突时在计划执行前报错。生成定义补齐 Python，并添加 `nodefaults`，使用清单声明的 channels；依赖交由 Conda 处理（`dependencies` 中的 `pip:` 子列表由 Conda 调用 pip 安装），不合并清单的 `dependencies.pip`。定义文件引用本身及 YAML 内的相对依赖输入均以清单目录为基准。

规范化定义写入 `.rospm/conda/environment.yml`。只有创建命令成功且 prefix 中存在 `conda-meta/history` 后，才写入 `.rospm/conda/ready.sha256`：第一行为规范化定义摘要，按锁重放（或 `lock` 记录过）的环境另有第二行锁文件摘要。普通 `apply` 与 `build` 只要求第一行匹配；`apply --locked` 要求两行都与锁记录一致。已存在但无匹配完成记录的目录（包括创建中断或定义变化）不会被覆盖，当前提示用户先移走目录再执行 `apply`；自动恢复与更新属于后续幂等性任务。该记录只表示创建完成及定义一致，不是包版本锁，也不检测用户手动修改包造成的漂移。

#### 受管环境的锁定与重放

锁记录分两层（决策 #4 的引用方式）：每个平台一份 Conda 显式锁（`conda list --explicit --sha256` 格式：`# platform: <subdir>`、`@EXPLICIT`，每行为带 MD5 / SHA256 摘要的包 URL），路径由 `lock_file` 决定，与清单一起提交；`rospm.lock` 的 `[[conda]]` 只记录平台、锁文件相对路径、锁文件摘要及规范化定义摘要，不复制包列表。普通 `apply` 始终按定义求解，不读取锁文件。

`rospm lock` 只为本机平台记录受管环境：要求环境存在且完成记录第一行与当前定义一致（否则说明并不记录，提示先 `apply`）；用选定的 Conda 执行 `conda list --explicit --sha256 --prefix <prefix>`，校验平台与摘要后写入锁文件（内容不变则不重写），随后在完成记录中补写锁文件摘要，使本机随后的 `apply --locked` 直接复用该环境。其它平台的 `[[conda]]` 记录与 apt 一样原样保留。

覆盖规则：显式锁只包含 Conda 包。定义中的 PyPI 依赖（`pip:` 子列表）必须全部写成 `name==version`（不接受范围、通配、URL、extras 或环境标记），重放时以 `pip install --no-deps` 安装，因此传递依赖也必须逐个固定；`lock` 用 `conda list --json` 核对环境中 channel 为 `pypi` 的包与这些固定完全一致（PEP 503 规范化名称），有未固定、多余或版本不同的包时不记录并列出。定义中的 `variables` 在重放后以 `conda env config vars set --prefix` 写入环境。

`apply --locked` 在规划阶段（不写文件、不启动子进程）检查：`rospm.lock` 有本机平台的 `[[conda]]` 记录；记录的定义摘要与当前规范化定义一致；锁文件存在且摘要一致；锁文件为本机平台、含 `@EXPLICIT` 且每个包都有摘要；锁覆盖定义中的每个 Conda 依赖名称、`python` 版本与清单一致，有 pip 依赖时含 `pip`；pip 依赖全部为精确固定。任一不满足即失败并提示在该平台 `apply` 后重新 `lock`，从不退回求解。执行时依次运行 `conda create --yes --prefix <prefix> --file <锁文件>`（显式文件不经求解器，Conda 校验包摘要）、写入变量、`conda run --no-capture-output -p <prefix> python -m pip install --no-deps --no-input --disable-pip-version-check <固定>`，全部成功后才写入两行完成记录。已有但完成记录不匹配锁的目录（例如此前按定义求解创建）不会被覆盖，仍提示先移走目录。

构建工具 / Python ABI / CMake 缓存检查及 `env` / `shell` 对接仍见后续任务。

### 2.3 叠加顺序的推导

1. 对 `workspaces` 按 `underlays` 做拓扑排序；有环则校验失败。
2. 顺序即：`apply` 的源码导入顺序、`rosdep install` 顺序、`build` 顺序。
3. `env` 生成时按该顺序依次 source 各 workspace 的 `install/setup.bash`，最底层为 ROS 底包。

## 3. 锁文件 `rospm.lock`

### 3.1 示例

```toml
schema_version = 1
generated_at = "2026-09-15T08:00:00Z"
manifest_sha256 = "…"                      # rospm.toml 的哈希，verify 时检测清单漂移

# ---------- 提供者层 ----------
[provider]
kind = "native"

[provider.native]
ubuntu = "24.04"
ros_snapshot = "2026-09-10"                # snapshots.ros.org/<distro>/<date>/ubuntu
ubuntu_snapshot = "20260910T000000Z"       # snapshot.ubuntu.com/ubuntu/<ts>

# provider = "pixi" 时改为：
# [provider.pixi]
# manifest = "pixi.toml"
# lock = "pixi.lock"
# lock_sha256 = "…"                        # 决策 #4：只记哈希，不复制内容

# provider = "docker" 时改为：
# [provider.docker]
# base_image = "ros:jazzy-ros-base"
# digest = "sha256:…"

# ---------- 源码层 ----------
[[sources]]
workspace = "main"
name = "navigation2"
type = "git"
url = "https://github.com/ros-navigation/navigation2.git"
rev = "3f2a…"                              # 与 `vcs export --exact` 等价

# ---------- apt 层（按平台分别记录） ----------
[[apt]]
platform = "linux-x86_64"
name = "ros-jazzy-navigation2"
version = "1.3.5-1noble.20260901.120000"
sha256 = "…"

[[apt]]
platform = "linux-aarch64"
name = "ros-jazzy-navigation2"
version = "1.3.5-1noble.20260901.120000"
sha256 = "…"

# ---------- pip 层（只记录，供 verify 对比，决策 #6） ----------
[[pip]]
name = "numpy"
version = "1.26.4"

# ---------- 受管 Conda 环境（按平台，只记引用与哈希，§2 受管环境的锁定与重放） ----------
[[conda]]
platform = "linux-x86_64"
lock = "conda-linux-64.lock"               # conda list --explicit --sha256，与清单一起提交
lock_sha256 = "sha256:…"
definition_sha256 = "sha256:…"             # 规范化环境定义的哈希
```

### 3.2 锁定严格度与降级

| 层 | 严格锁定 | 降级 | 降级时输出 |
|---|---|---|---|
| git | commit SHA | 无 | — |
| apt | 版本 + 双快照 | 快照不可达 → 仅校验版本，不承诺重装 | `WARN: apt snapshot unreachable, falling back to version check` |
| pixi | `pixi.lock` 哈希 | 无 | — |
| docker | digest | 无 | — |
| pip | 无 | 始终仅记录 | — |
| 受管 Conda | 显式锁（每包摘要）+ 锁文件哈希 + 定义哈希 | 无：缺记录、摘要或定义不符直接失败，不重新求解 | — |

**`apply --locked` 的做法（P2）**：

- 前置：`rospm.lock` 必须存在且 `manifest_sha256` 与清单原文一致，`provider.kind` 须为 native，锁文件里须有本机平台的 apt 记录且含清单的底包；任一不满足直接报错提示重新 `rospm lock`（陈旧的锁是用户错误，不是降级）。
- ROS 源：复用 P1 的源文件管理——同一个 `/etc/apt/sources.list.d/ros2.list` 改写为 `deb [arch=<arch> signed-by=/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg] http://snapshots.ros.org/<distro>/<date>/ubuntu <codename> main`；keyring 先从 `keyserver.ubuntu.com` 下载、按指纹 `4B63…CBF125EA` 核验，keyserver 返回的是 ASCII armor 格式，须 `gpg --dearmor` 成二进制后才安装（apt 不接受 `.gpg` 路径下的 armor 格式文件）；已存在且为二进制则跳过，armor 格式的旧文件会被重装。文件已指向同一快照则不动；文件不是 rospm 写的（如 `ros2-apt-source`）则不动并说明快照未应用。
- Ubuntu 源：写 rospm 自己的 `/etc/apt/sources.list.d/rospm-ubuntu-snapshot.list`（`<codename>`、`<codename>-updates`、`<codename>-security` × `main restricted universe multiverse`，`signed-by=ubuntu-archive-keyring.gpg`），**不改** Ubuntu 自身的 `sources.list` / `ubuntu.sources`（决策 #17）。这个文件只是让锁定的版本可下载；写入后立即 `apt-get update`，否则锁定安装只看得到实时源索引。
- 安装：`apt-get install -y --allow-downgrades <名>=<版本> …`，逐条列出锁文件里本机平台的记录（每条命令最多 100 个包），替代底包 / 额外 apt 两步。ROS 层（`ros-<distro>-*`、底包）与三个工具包总是精确安装，必要时降级；其余 Ubuntu 包若本机已装且版本更新（`dpkg --compare-versions` 规则），保持不变、计划中列出，不降级——锁文件只记录顶层包，降级 `curl` 需要同版本的 `libcurl4`，apt 会让整条命令失败（2026-09-26 实测），且回退安全更新不可取。`verify` 对这种情况报 `info`。`rosdep init` / `update` 照常，`rosdep install` 跳过并说明（锁文件已是解析结果）。源码 `vcs import` 后逐仓库 `git checkout --detach <rev>`；非 git 源不固定并说明。
- 降级：锁文件中快照为空，或真实运行前 GET 快照 `InRelease` 不通（10 s），该层退回实时源 / 镜像，并打印上表的 `WARN` 行与一条说明；版本仍按锁文件安装。`--dry-run` 不探测网络。禁止 `trusted=yes`、禁止关闭签名检查；密钥过期（`EXPKEYSIG`）提示从 keyserver 刷新。
- 计划起始的 `apt-get update` 之前，先 `rm -f` 本次会重写的 rospm 自有源文件（`ros2.list` 需要改指向或 keyring 需要重装、`rospm-ubuntu-snapshot.list` 时间戳不同）：上次失败的 `apply --locked` 可能留下不可用的源，否则起始 update 就失败、修复步骤轮不到执行。不是 rospm 写的文件与无需重写的文件不动。
- `uninstall` 连同 `ros2.list` 一并删除快照 keyring 与 `rospm-ubuntu-snapshot.list`；`ros2.list` 已不存在时（例如中断的 `apply --locked` 之后）也删除这两个残留，`--keep-source` 时保留。

### 3.3 `lock` 的解析规则（P2）

`rospm lock` 只读地解析**本机**，写出 `rospm.lock`；它描述的是已经 `apply` 过的状态，不是意图。

| 层 | 来源 | 未满足时 |
|---|---|---|
| 源码 | 按叠加顺序对每个带 `sources` 的 workspace 运行 `vcs export --exact src`，与 `.repos` 中声明的仓库逐一对应：`rev` 取导出的提交 SHA，`url` 取 `.repos` 里的写法（检出的 origin 不同只加说明），`version` 记录 `.repos` 请求的分支 / 标签供人阅读。vcs 缺失或导出失败时对 git 仓库退回 `git rev-parse HEAD` 并说明 | 任一声明的仓库未导入 → 报错列出（先 `rospm apply`）；vcs 与 git 都没有 → 报错 |
| apt | 本机 dpkg：全部已安装的 `ros-<distro>-*`、底包、平台覆盖后的额外 apt 包、`python3-rosdep` / `python3-vcstool` / `python3-colcon-common-extensions`，按名字排序；`.deb` 的 sha256 尽力从 `apt-cache show <名>=<版本>` 取，取不到留空 | 底包 / 额外包 / 工具未安装 → 不记录并说明；`dpkg-query` 不可用 → 报错；主机 Ubuntu 与清单不符 → 报错（与 `apply` 同一规则，否则记录的版本对不上快照） |
| 快照 | ROS：GET `snapshots.ros.org/<distro>/` 目录索引，取最新的日期，读其 `dists/<codename>/Release` 核对 `Suite` / `Architectures` 含本机架构，下载 `main/binary-<arch>/Packages.gz` 核对本机全部锁定的 `ros-<distro>-*` 版本都在其中，再用 keyserver 取得并按指纹 `4B63…CBF125EA` 核验的密钥 `gpgv` 验证 `Release.gpg`；最新的三个日期依次尝试，都不含锁定版本（本机在最新快照之后从实时源安装）则不记录 ROS 快照并列出缺失的版本，提示下一个快照发布后重新 `lock`。Ubuntu：取本机 `/var/lib/dpkg/status` 的修改时间（最后一次装 / 卸包，此时本机装的版本都在实时源中；重复 `lock` 不变，保持幂等），取不到才退回 ROS 快照 `Release` 的 `Date:`，换算成 `YYYYMMDDTHHMMSSZ`，GET `snapshot.ubuntu.com/ubuntu/<ts>/dists/<codename>/InRelease` 须 200，并用 `/usr/share/keyrings/ubuntu-archive-keyring.gpg` 验签（无该文件则跳过并说明）。gpg / gpgv 缺失时记录日期但说明"签名未检查"；签名**失败**则不记录该日期。之后其余锁定包（不在 ROS 快照索引中的）逐个在 Ubuntu 快照的 `<codename>-security` / `-updates` / `<codename>` × `main universe restricted multiverse` 索引中查找（找齐即停，`universe` 索引有数十 MB），仍找不到的列出数量与示例；索引下载失败则说明未能核对 | 任一层失败 → 该字段为空 + `DEGRADED` 说明（`apply --locked` 用实时源）；`ROSPM_NO_NETWORK=1` 跳过整层并说明 |
| pip | `python3 -m pip list --format=freeze`，只记录 `[dependencies].pip` 里点名的包（PEP 503 规范化名字） | pip 不可用或包未装 → 不记录并说明（决策 #6） |
| 受管 Conda | 已就绪的受管环境：`conda list --explicit --sha256` 写入 `lock_file`，记录其哈希与定义哈希（§2 受管环境的锁定与重放） | 未就绪、PyPI 包未精确固定或与固定不一致、Conda 查询失败 → 不记录并说明（`apply --locked` 在该平台失败）；外部环境 → 不记录并说明 |

- 只能解析本机平台：`--platform` 与本机不同直接报错（P5 起查仓库索引）。已有锁文件中**其它平台**的 apt 记录原样保留，本平台的记录整体替换，因此一份锁文件可由多台机器各自 `lock` 累积。
- 幂等：解析结果与现有文件除 `generated_at` 外完全一致时不重写文件、`generated_at` 保持，输出 `unchanged`。apt 按（平台，名字）排序，源码按叠加顺序与 `.repos` 顺序。
- `manifest_sha256` 是 `--manifest` 所指文件的原文哈希。

### 3.4 `verify` 的对比规则（P2）

只读。输出与 `check` 相同的期望 vs 实际表：

| 区域 | 对比 | 状态 |
|---|---|---|
| manifest | 清单原文哈希 vs `manifest_sha256`；`environment.provider` vs `provider.kind` | 不一致 → `MISMATCH`（提示重新 `rospm lock`） |
| os | `provider.native.ubuntu` vs `/etc/os-release`；两个快照日期只作 `info` 展示（是否已切到快照源由 `apply --locked` 负责） | 不同 → `MISMATCH`，不可读 → `unknown` |
| source | 每条 git 记录：目录不存在 → `MISSING`；不是 git 检出 → `MISMATCH`；`HEAD` ≠ `rev` → `MISMATCH`（同时给出两个短 SHA）；origin 与 `url` 宽松比较不同 → 额外一行 `MISMATCH`；非 git 类型 → `info` 未核对；锁文件里的 workspace 不在清单 → `MISMATCH` | git 缺失 → `unknown` |
| apt / ros | 只核对本机平台的记录：未安装 → `MISSING`，版本不同 → `MISMATCH`（ROS 层以外的 Ubuntu 包已装版本更新 → `info`，与 `apply --locked` 不降级一致）；其它平台的记录合并成一行 `info`；底包一行归 `ros` 区域；锁文件没有本机平台的底包记录 → 底包一行 `MISSING`（`apply --locked` 会拒绝这样的锁） | `dpkg-query` 不可用 → 每包 `unknown` |
| pip | 与 `pip list --format=freeze` 对比：未装 → `MISSING`，版本不同 → `MISMATCH` | pip 不可用 → `unknown` |

退出码：全部一致（无 `MISSING` / `MISMATCH` / `unknown`）为 0，否则 1（供 CI 用）。`--json` 形如 `{ ok, manifest, lock, generated_at, manifest_matches, ros_snapshot, ubuntu_snapshot, environment: { probed, ready, problems, unknown, items } }`。

## 4. CLI 动词表

命令名 `rospm`。分级依据决策 #2。

| 命令 | 级别 | 作用 | `--dry-run` |
|---|---|---|---|
| `init [--from-env] [--distro <名>] [--base ros-base\|desktop]` | 写项目文件 / 幂等 | 生成清单模板；`--distro humble\|jazzy\|rolling` 指定模板发行版，不指定时 Ubuntu 22.04 → Humble、24.04 → Jazzy，无法识别时 → Jazzy；`--from-env` 尽力探测（决策 #3），不与 `--distro` 同用；`--base` 选底包元包（默认 `ros-base`，`desktop` 含 RViz / demos），写入 `environment.native.ros_base`；已存在不覆盖，需 `--force` | — |
| `check [--target <platform>] [--strict]` | 只读 | 校验清单、拓扑排序、探测底包 / rosdep / apt / 源码差异并输出期望 vs 实际表；显式 `--target` 查询 apt / conda 目标仓库索引，跨架构时跳过主机探测；默认只报告（退出码 0），`--strict` 在所执行的检查有缺失或未知项时退出 1 | — |
| `lock [--platform <p>]` | 写项目文件 / 幂等 | 把本机解析成 `rospm.lock`（§3.3）：源码提交、apt 版本、pip 版本、经签名验证的 ROS / Ubuntu 快照日期、受管 Conda 环境的显式锁（写入 `lock_file`）；同一输入产生同一输出（内容不变则不重写、`generated_at` 保持）；只能解析本机平台，其它平台的 apt 与 Conda 记录原样保留；`ROSPM_NO_NETWORK=1` 跳过快照探测（降级并告警） | — |
| `verify` | 只读 | 对比锁文件与当前环境（§3.4）：清单哈希、提供者、Ubuntu 版本、每个源码检出的 HEAD 与 origin、本机平台的 apt 版本、pip 版本；输出与 `check` 同样的期望 vs 实际表，快照日期作 `info` 展示；全部一致退出 0，有 MISSING / MISMATCH / unknown 退出 1；清单哈希不一致提示重新 `lock` | — |
| `apply [--distro <名>] [--locked] [--offline] [--mirror <name>]` | 修改系统 | `--distro humble\|jazzy\|rolling` 与清单不同时，先核对主机 Ubuntu（Humble → 22.04，Jazzy / Rolling → 24.04），不符或无法识别则报错且不改文件；相符则将 `project.ros_distro` 写回清单，`ros_base` 的 `ros-<旧发行版>-` 前缀同步改名，显式的 `environment.native.ubuntu` 同步为目标版本，保留注释；`--locked` 下发行版不同直接报错，提示先 `apply --distro` 再 `lock`。随后测速（§4.1.1，`--dry-run` / `--offline` 跳过），再按顺序：ROS apt 源（见 §4.1）→ 底包 → 源码导入 → rosdep → 额外 apt；主机 Ubuntu 版本与清单不符直接报错；其他已装发行版只在计划中说明并保留（`uninstall` 只卸清单发行版）；`--dry-run` 只预览，包括清单写回；`--locked` 要求 `rospm.lock` 存在且与清单哈希一致（否则报错提示 `rospm lock`），把源切到快照并按锁定版本安装（§3.2），受管 Conda 环境按本平台显式锁重放、缺记录即失败（§2）；`--mirror official\|tuna\|ustc\|aliyun`（或 `ROSPM_MIRROR`）指定源集，`--locked` 下只作用于 rosdistro / rosdep，快照没有镜像 | 必须 |
| `uninstall [--keep-source] [--yes]` | 修改系统 | 卸载清单所述 ROS 发行版：显式列出并 `apt-get remove` 全部已装 `ros-<distro>-*`，`autoremove`；随后删除 rospm 写入的 apt 源与 keyring（`--keep-source`、机器上还有其它发行版、或源文件非 rospm 所写时保留）。不动项目文件、源码与 workspace，不卸 rosdep / vcstool / colcon。非 `--dry-run` 须交互确认或 `--yes`，stdin 非终端且无 `--yes` 时拒绝执行 | 必须 |
| `build [<workspace>] [--conda-env <name> \| --conda-prefix <path>]` | 产出产物 | 按拓扑顺序调用 colcon，注入清单中的 colcon 配置；可覆盖项目默认的 Conda 环境选择 | 必须 |
| `env [--shell bash\|zsh\|fish]` | 只读 | 输出可 `source` 的环境脚本 | — |
| `shell` | 启动子进程 | 在已 source 的子 shell 中工作 | — |
| `deps check` / `deps install` | 只读 / 修改系统 | `check`：解析全部 workspace 的 `package.xml`，把依赖键分为内部（项目内某个包提供）、外部（交给 rosdep）、跳过（`skip_keys`）；外部键经 `rosdep check` / `rosdep resolve` 映射到 apt 包并查 dpkg，输出期望 vs 实际表（未装 `MISSING`、rosdep 无法解析或未安装 rosdep 为 `unknown`），`package.xml` 解析失败为 `MISMATCH`；始终退出 0；`--json` 为 `{ ok, workspaces[], internal[], external[], skipped[], environment }`。`install`：与 `apply` 的 rosdep 步骤同一命令（未 `rosdep update` 时先 update，按叠加顺序逐 workspace `rosdep install`，`--skip-keys`，`ROSPM_MIRROR` 生效），但不做 `rosdep init`（提示先 `rospm apply`）、不测速 | install 必须 |
| `export [--target <p>] [-o <path>]` | 产出产物 | 生成离线包，见 §5 | 必须 |
| `import <bundle>` | 修改系统 | 校验并解包，然后等价于 `apply --locked --offline` | 必须 |
| `editor [vscode\|clion\|devcontainer]` | 写项目文件 / 幂等 | 生成编辑器配置 | — |
| `doctor [--no-network]` | 只读 | 自检工具依赖（vcs、colcon、rosdep、pixi、docker）是否可用；ROS apt 源 / `universe` / locale 状态；官方源与各镜像的测速表（§4.1.1） | — |

全局选项：`--manifest <path>`、`--verbose`、`--json`（机器可读输出，供 GUI 与 CI 使用）、`--lang zh|en`（P1.5 起，见 §4.2）。

`check --target aarch64`（也接受 `linux-aarch64` / `arm64`）的仓库检查独立输出为 JSON 的 `repositories.items`；未指定 `--target` 时该字段为 `null`，不联网查询索引。native 查询官方 ROS 与 Ubuntu 的目标架构索引（arm64 使用 Ubuntu ports），覆盖 release / updates / security 的四个组件，检查平台覆盖合并后的 apt 包、底包及三个构建工具包。pixi 读取配置的 `pixi.toml`，不存在则在内存中生成默认配置；查询各 channel 的目标子目录及 `noarch` 的完整 `repodata.json.bz2`，包含普通、host、build 与适用 target 依赖，暂不支持 feature 环境。需安装 `gzip`（apt）或 `bzip2`（conda）。

这里仅检查包名存在性，不求解版本约束、虚拟包或传递依赖，也不代表目标主机已安装或可成功构建。成功索引中存在包即报告 `ok`；全部索引查询成功仍找不到才报告 `missing`；下载、解压、解析失败或 `ROSPM_NO_NETWORK=1` 则报告 `unknown`，`--strict` 退出 1。该检查不修改主机 apt 源、不注册额外架构，也不替代包管理器安装时的签名验证。

### 4.1 native 提供者：`apply` 的 ROS 安装（bootstrap）

native 是唯一不自带 ROS 的提供者（pixi 由 RoboStack 安装，docker 由基础镜像提供），因此"安装 ROS"属于 native `apply` 的范围。做法只封装官方安装文档的命令，不做任何新东西：

| 步骤 | 命令 | 跳过条件（由 `check` 同一探测决定） |
|---|---|---|
| 0 | 校验主机 | `/etc/os-release` 的 Ubuntu 版本 ≠ `environment.native.ubuntu` → 报错退出；不可读 → 告警后按清单假定 |
| 1 | `apt-get update`；安装前置包 `locales` / `software-properties-common` / `curl ca-certificates gnupg` | 只安装下面步骤实际需要的 |
| 2 | `locale-gen en_US en_US.UTF-8` + `update-locale LANG=en_US.UTF-8`（只设 `LANG`，不设 `LC_ALL`） | **系统** locale 已是 UTF-8：`/etc/default/locale` 中 `LC_ALL` / `LC_CTYPE` / `LANG` 的首个非空值含 `UTF-8`；无该文件时才看当前进程环境 |
| 3 | `add-apt-repository -y universe` | 任一 apt 源文件（一行式或 deb822）已启用 `universe` |
| 4 | 下载 `ros.key`（`curl -f`，连接 15s / 单次 60s 超时，瞬时错误重试 2 次）到临时文件，用 `gpg --show-keys` 核对完整指纹 `C1CF 6E31 E6BA DE88 68B1 72B4 F42E D6FB AB17 C654`（Open Robotics，有效期至 2030-06-01），一致才 `sudo install` 到 `/usr/share/keyrings/ros-archive-keyring.gpg`；写 `/etc/apt/sources.list.d/ros2.list`（`deb [arch=<dpkg arch> signed-by=…] http://packages.ros.org/ros2/ubuntu <codename> main`）；`apt-get update` | 任一 apt 源文件已引用 `packages.ros.org/ros2`（含 `ros2-apt-source` 包写的 `ros2.sources`） |
| 5 | `apt-get install -y <ros_base> python3-rosdep python3-vcstool python3-colcon-common-extensions` | 无（apt 自身幂等） |

- rospm 自己写 keyring + `sources.list.d`，而不是安装 `ros2-apt-source` 包：P2 `apply --locked` 要把同一文件指向 `snapshots.ros.org` 并以 `signed-by=` 引用快照密钥，两者共用一套源文件管理代码（决策 #14）。
- locale 只看**系统默认值**（`/etc/default/locale`，PAM 登录时读取、`update-locale` 写入），不看调用 rospm 的进程环境：`LANG=C rospm apply`（CI、脚本、临时切英文输出）不得改写用户的系统 locale。文件里非 UTF-8 的值来自 `LC_ALL` / `LC_CTYPE` 时，`update-locale` 以裸变量名把它移除（注释掉）而不是覆盖，让新的 `LANG` 生效。没有 `/etc/default/locale`（裸容器）时退回进程环境判断。`check` / `doctor` 分两行：`system locale`（`apply` 会修的那个，非 UTF-8 为 `MISSING`）与 `shell locale`（仅当前 shell，非 UTF-8 为 `info`，`--strict` 不受影响）；计划注释写明改的是哪个文件、原值是什么。rospm 自身的输出语言选择（§4.2）与此无关，仍只看进程环境。
- `--dry-run` 会完整打印将写入的源文件内容。
- 密钥指纹内置于二进制（`probe::ROS_KEY_FINGERPRINT`）。指纹不符或下载失败（404、超时）时 `apply` 报错退出，不写 keyring；不能因为网络停滞无限等待（P1 实测中 `curl` 曾在 HTTP/2 连接上挂起 2 分钟以上）。
- `[workspaces.<name>.colcon].mixins` 依赖 `python3-colcon-mixin`，该包不在 `python3-colcon-common-extensions` 的依赖里，且 mixin 仓库需要用户自行 `colcon mixin add/update`（决策 #8）。清单声明了 mixins 时，`check` 额外探测 `colcon mixin` 是否可用。
- 不做：安装 / 升级 / 更换 Ubuntu 本身，内核、驱动（§7）。

#### 4.1.1 源镜像与网络探测（决策 #17）

内置源集（`rospm-core::mirror`）：

| 名称 | ROS apt | rosdistro（`ros.key`、`index-v4.yaml`、rosdep yaml） |
|---|---|---|
| `official` | `http://packages.ros.org/ros2/ubuntu` | `https://raw.githubusercontent.com/ros/rosdistro/master` |
| `tuna` | `https://mirrors.tuna.tsinghua.edu.cn/ros2/ubuntu` | `https://mirrors.tuna.tsinghua.edu.cn/rosdistro` |
| `ustc` | `https://mirrors.ustc.edu.cn/ros2/ubuntu` | `https://mirrors.ustc.edu.cn/rosdistro` |
| `aliyun` | `https://mirrors.aliyun.com/ros2/ubuntu` | 无，回退官方 |

**探测**（`rospm-core::net`）：非 `--dry-run` 的 `apply` 在执行任何步骤之前，并行 GET 每个源集的 `dists/<codename>/InRelease` 与 `rosdistro/rosdep/base.yaml`（约 270 KB，用来测吞吐并发现"能建连但中途停滞"的连接），以及 `.repos` 中每个 git 主机的根页面；每个 URL 8 s 超时，结果打成表（`ok 144 ms 4.6 KB [KB/s]` / `FAILED 原因`）。`doctor` 打印同一张表（`--no-network` 跳过）。

**判定**：官方两项都可达 → 用官方。否则在"所需端点全部可达"的镜像里按总延迟排序（`aliyun` 只有 apt，需官方 rosdistro 可达才算候选）：终端交互时询问 `use the <name> mirror …? [Y/n]`，非终端直接报错并列出可用的 `--mirror <name>`；一个都不可达则报错提示检查网络 / 代理。`--mirror <name>` / `ROSPM_MIRROR` 显式指定时只探测该源集，不可达则报错（`--mirror official` 例外：告警后继续）。git 主机不可达只告警（没有镜像）。

**换源的实际效果**：`ros.key` 从镜像的 `rosdistro/ros.key` 下载，仍按内置指纹校验；`ros2.list` 指向镜像 apt；rosdep 不跑 `rosdep init`，而由 rospm 写 `20-default.list`（镜像自己的那份仍指向 GitHub，不能直接用），且不含上游的 `gbpdistro … fuerte` 行（rosdep 处理该行时会从硬编码的 GitHub 地址取 `targets.yaml`，实测在 GitHub 不通时仅此一行就让 `rosdep update` 失败；ROS 2 用不到 fuerte），`rosdep update` 带 `ROSDISTRO_INDEX_URL=<镜像>/index-v4.yaml`。`ros2.list` 若是 rospm 自己写的且指向别的源集则重写；若是别人写的（如 `ros2-apt-source` 的 `ros2.sources`）则保留并注明。`uninstall` 不动 `20-default.list`（rosdep 属于保留的工具）。

**不做**：不改 Ubuntu 自身的 apt 源（用户系统配置）；不为 git 提供镜像；`--dry-run` 不测速（保持离线可用与确定性，可用 `--mirror` 看镜像版计划）。计划中官方源步骤失败时的错误信息会提示 `rospm doctor` 与 `--mirror`。

### 4.2 输出语言（i18n，P1.5，决策 #16）

**语言选择**：按 GNU gettext 的优先级读环境变量 `LANGUAGE`（冒号分隔列表，取第一个能匹配的）→ `LC_ALL` → `LC_MESSAGES` → `LANG`。值以 `zh` 开头（`zh_CN.UTF-8`、`zh_TW`、`zh_Hans` 等）→ 简体中文；其余（含未设置、`C`、`POSIX`）→ 英文。`ROSPM_LANG=zh|en` 覆盖环境 locale，`--lang` 再覆盖 `ROSPM_LANG`。语言在解析命令行参数之前确定，因此 `--help` 也随之切换。

**翻译范围**（初期只有 `zh-CN` 与 `en`，`en` 为源语言）：

| 翻译 | 不翻译 |
|---|---|
| clap 的命令 / 参数说明与 `--help` | 子命令名、参数名、`rospm.toml` / `rospm.lock` 的键与值 |
| `check` / `doctor` / `init` 的表格文字、提示、`detected` / `TODO` 说明 | `--json` 的键名、`status` 等枚举值（`ok` / `missing` / …） |
| 计划里的 `# 注释行`（降级告警、说明） | 计划里的 shell 命令本身与将写入的文件内容 |
| 错误信息（`Error` 的 Display）与确认提示 | `tracing` 日志（`-v`，面向开发者）、外部工具（apt / rosdep / colcon）的原始输出 |
| `init` 写出的清单模板注释（按语言选模板） | 文档 `docs/`（另行维护） |

**实现约束**：
- 翻译资源随二进制编译（单文件分发不变），运行时不读外部文件。
- `--json` 中的自由文本字段（如 `actual`、`notes`）也随语言变化；需要稳定文本的消费者应看枚举字段，或设 `ROSPM_LANG=en`。CI 用英文（runner 的 locale 是 `C`）。
- 英文单复数（`1 package` / `2 packages`）用独立键或改写句式，不依赖库的复数规则；中文无此问题。
- 每个键必须同时存在于两种语言，由单元测试保证；缺失键回退英文并在 `-v` 下告警，不 panic。
- P1.5 之后新增的面向用户字符串一律走 i18n 层；review 时把裸英文字符串视为缺陷。

**实现说明（P1.5 落地后，2026-09-26）**：
- 库：`rust-i18n`，资源 `crates/rospm-cli/locales/{en,zh-CN}.yml` 编译进二进制。`rospm-core` 只产出 `Message`（`Text` 原样文本，或 `Key { key, args }`），`plan` 注释 / 标题、`check` 报告的 `subject` / `expected` / `actual`、`Error`、`init --from-env` 的说明与草稿注释全部是 `Message`；CLI 启动时通过 `rospm_core::message::set_renderer` 安装一个渲染函数，`Message` 的 `Display` / `Serialize` 由它完成，未安装时显示开发者形式 `key(arg=value)`（核心库的单测看到的就是这种形式）。
- `LANGUAGE` 列表中只有 `zh*` / `en*` 条目算"匹配"，其它条目跳过；列表无匹配时继续按 `LC_ALL` → `LC_MESSAGES` → `LANG` 判断。`--lang` 只接受 `zh` / `en`。
- `--help` 的章节标题（用法 / 命令 / 参数 / 选项）通过自定义 help 模板翻译，`help` 子命令由 rospm 自己实现；但 clap 自身的解析错误（如 `error: unexpected argument`、`invalid value`）与 `[OPTIONS]` / `<COMMAND>` 这类占位符仍为英文，属已知限制。
- 英文单复数用独立键（如 `probe.workspace.one_package` / `n_packages`），未用库的复数规则。

## 5. 离线导出包格式

决策 #10：`manifest + data`，校验必选，签名可选，压缩可选。

```
<name>-<platform>-<date>.rospm/           # 或 .rospm.tar / .rospm.tar.zst
  rospm.toml
  rospm.lock
  bundle.toml                              # 导出元数据：目标平台、生成时间、工具版本、压缩方式
  sources/
    <workspace>/<repo>.bundle              # git bundle，导入后 `git clone` 到指定 rev
  payload/
    apt/                                   # 本地 apt 仓库：pool/ + dists/…/Packages.gz
    pixi/                                  # pixi-pack 产物（仅 pixi provider）
    docker/image.tar                       # docker save（仅 docker provider）
  SHA256SUMS                               # 必选，覆盖上述全部文件
  SHA256SUMS.sig                           # 可选，minisign 或 ssh-keygen -Y 签名
```

规则：
- `import` 先校验 `SHA256SUMS`，有 `.sig` 且用户提供公钥时校验签名；校验失败直接终止。
- 跨架构导出在 x86 上完成：apt 走 `dpkg --add-architecture` + `apt-get download <pkg>:arm64`；pixi 走 `pixi-pack --platform linux-aarch64`；docker 走 `buildx --platform linux/arm64` 后 `save`。
- 导入端走 `apply --locked --offline`，apt 源指向 `payload/apt`。

## 6. 测试矩阵

决策 #11。全部在容器内运行。

| 维度 | 取值 |
|---|---|
| 架构 | x86_64、aarch64（GitHub ARM runner，缺失时 QEMU） |
| Ubuntu / ROS | 22.04 + Humble、24.04 + Jazzy；Rolling 仅冒烟。发行版 EOL 后矩阵不缩减 |
| 提供者 | native（P1 起）、pixi（P3 起）、docker（P4 起） |
| 场景 | `check` → `apply --dry-run` → `apply` → `build` → `env` 后 `ros2 run` 冒烟；P2 起加 `lock` → 新容器 `apply --locked` → `verify` |

单元测试重点：清单解析与校验、拓扑排序、平台覆盖合并、锁文件读写往返、bundle 校验。

## 7. 非目标（记录，决策 #9）

- ROS 1、非 Ubuntu 平台、Windows / macOS
- 安装、升级或更换 Ubuntu 本身（native `apply` 只在版本匹配的主机上安装 ROS，否则报错）
- 硬件驱动、内核模块、udev 规则、固件
- CUDA / GPU 驱动版本管理（只能通过 `target.replace` 声明替代包）
- pip 包的安装与锁定（只记录）
- 替代 colcon / rosdep / vcstool / pixi / docker 的任何功能（只封装）
- Nix 路线
- 从已有环境完整还原 colcon 配置与叠加顺序（只 best-effort）

## 8. P0 外部事实验证状态

实测过程、命令、证据及未完成项见 [P0 核验报告](p0-external-validation.md)。

- ROS：日期目录和 Jazzy noble 的 amd64/arm64 索引已读取，索引大小及 SHA256 与 Release 一致；快照间隔不固定，必须发现实际日期；当前 Rolling 路径 404。快照由独立密钥 `4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA`（"ROS Snapshot builder"，官方 wiki 指定从 `keyserver.ubuntu.com` 获取，有效期至 2027-06-01）签名，`gpgv` 验证 Release.gpg / InRelease 通过；`apply --locked` 必须以 `signed-by=` 引用该 keyring。
- Ubuntu：22.04 的 APT **2.4.11 起已回移植快照支持**；本机 2.4.14 的原生 `--snapshot` 与直接 URL 两种方案均更新成功，直接 URL 下载指定版本样例包成功。
- pixi-pack：pixi 0.81.0 + pixi-pack 0.7.11 在 x86_64 上为 `linux-aarch64` 完成 RoboStack 最小项目的 `pixi lock` 与打包，归档内 34 个包 SHA256 与 `pixi.lock` 全部一致；ARM 主机解包运行待 P3/P5。
- rosdep：RoboStack 默认列表加载 `ros-noetic/master/conda.yaml`；使用独立源/缓存及 `--os=conda:linux`。系统键映射已实测；ROS 包映射依赖 conda-forge 补丁：未经补丁的上游对 Humble `rclcpp` 解析失败，conda-forge 0.26.0 build 1 解析为 `ros-humble-rclcpp`。该构建的 `check`/`install` 仅兼容 micromamba 1.x，rospm 只用其 `resolve`/`keys`。`skip_keys` 共用原始键，但 Pixi keys/resolve 流程须在解析前自行过滤，安装由 Pixi 执行。
