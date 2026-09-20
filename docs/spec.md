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
ros_base = "ros-jazzy-ros-base"            # 底包，也可为 ros-jazzy-desktop

# provider = "pixi" 时改为：
# [environment.pixi]
# manifest = "pixi.toml"                   # 仅引用；pixi.lock 与之同目录

# provider = "docker" 时改为：
# [environment.docker]
# base_image = "ros:jazzy-ros-base"
# dockerfile = "docker/Dockerfile"         # 省略则由工具生成到 .rospm/docker/
# platforms = ["linux/amd64", "linux/arm64"] # buildx

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
| `workspaces.<name>` | 至少一个 | `path` 相对于清单所在目录；`sources` 可省略（纯本地源码） |
| `workspaces.<name>.underlays` | 否 | 引用的 workspace 必须存在；整体必须是 DAG |
| `dependencies.pip` | 否 | 只记录，`apply` 不执行 |
| `target.<platform>.*` | 否 | `<platform>` 必须在 `project.platforms` 内；合并顺序：追加 → 替换 → 排除 |
| `env.*` | 否 | 字符串；工具保留 `AMENT_PREFIX_PATH`、`COLCON_PREFIX_PATH` 由叠加顺序自动生成，用户声明时报错 |

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
```

### 3.2 锁定严格度与降级

| 层 | 严格锁定 | 降级 | 降级时输出 |
|---|---|---|---|
| git | commit SHA | 无 | — |
| apt | 版本 + 双快照 | 快照不可达 → 仅校验版本，不承诺重装 | `WARN: apt snapshot unreachable, falling back to version check` |
| pixi | `pixi.lock` 哈希 | 无 | — |
| docker | digest | 无 | — |
| pip | 无 | 始终仅记录 | — |

## 4. CLI 动词表

命令名 `rospm`。分级依据决策 #2。

| 命令 | 级别 | 作用 | `--dry-run` |
|---|---|---|---|
| `init [--from-env]` | 写项目文件 / 幂等 | 生成清单模板；`--from-env` 尽力探测（决策 #3）；已存在不覆盖，需 `--force` | — |
| `check [--target <platform>]` | 只读 | 校验清单、拓扑排序、探测底包 / rosdep / apt / 源码差异；`--target` 查目标架构包可用性 | — |
| `lock [--platform <p>]` | 写项目文件 / 幂等 | 生成 `rospm.lock`；同一输入产生同一输出 | — |
| `verify` | 只读 | 对比锁文件与当前环境，报告漂移；清单哈希不一致则提示重新 `lock` | — |
| `apply [--locked] [--offline]` | 修改系统 | 按顺序：底包 → 源码导入 → rosdep → 额外 apt；`--locked` 切换快照源 | 必须 |
| `build [<workspace>]` | 产出产物 | 按拓扑顺序调用 colcon，注入清单中的 colcon 配置 | 必须 |
| `env [--shell bash\|zsh\|fish]` | 只读 | 输出可 `source` 的环境脚本 | — |
| `shell` | 启动子进程 | 在已 source 的子 shell 中工作 | — |
| `deps check` / `deps install` | 只读 / 修改系统 | 解析全部 `package.xml`，与已安装集合求差；`install` 交给 rosdep 执行 | install 必须 |
| `export [--target <p>] [-o <path>]` | 产出产物 | 生成离线包，见 §5 | 必须 |
| `import <bundle>` | 修改系统 | 校验并解包，然后等价于 `apply --locked --offline` | 必须 |
| `editor [vscode\|clion\|devcontainer]` | 写项目文件 / 幂等 | 生成编辑器配置 | — |
| `doctor` | 只读 | 自检工具依赖（vcs、colcon、rosdep、pixi、docker）是否可用 | — |

全局选项：`--manifest <path>`、`--verbose`、`--json`（机器可读输出，供 GUI 与 CI 使用）。

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
- 硬件驱动、内核模块、udev 规则、固件
- CUDA / GPU 驱动版本管理（只能通过 `target.replace` 声明替代包）
- pip 包的安装与锁定（只记录）
- 替代 colcon / rosdep / vcstool / pixi / docker 的任何功能（只封装）
- Nix 路线
- 从已有环境完整还原 colcon 配置与叠加顺序（只 best-effort）

## 8. P0 待验证事项

- `snapshots.ros.org` 的目录格式与 Jazzy 快照覆盖频率；Rolling 是否有快照。
- `snapshot.ubuntu.com` 在 22.04 上的可用方式（apt 2.7.14+ 才有原生 `Snapshot:` 支持，22.04 需直接写 URL）。
- `pixi-pack` 对 RoboStack channel 与 `--platform linux-aarch64` 的支持程度。
- rosdep 在 RoboStack 下的 yaml 源配置方式，以及能否与 apt 侧共用同一份 `skip_keys`。
