# Manifest / lock schema v1

冻结日期：2026-10-25。本文与 [spec.md §2–3](spec.md) 定义磁盘格式 v1；实现为 `crates/rospm-core/src/{manifest,lock}.rs`，固定样本位于 `crates/rospm-core/tests/fixtures/schema-v1/`，契约测试为 `tests/schema_v1.rs`。冻结基线为引入本文与这些 fixtures 的 Git 提交（可用 `git log --diff-filter=A -- docs/schema-v1.md` 定位），不另外创建发布 tag。软件版本已设为 `1.0.0`，格式冻结不代表 1.0 发布或原生 ARM 验收完成。

当前没有实际用户，不提供冻结前数据迁移或向后兼容方案。以后改变这里冻结的字段、类型或默认语义须显式评审 schema 版本，不能仅修改 Rust 类型而继续声称满足此基线。CLI JSON、Conda 内部完成标记、未来运行配置及离线 bundle 不属于本次冻结。

## 通用规则

- 格式为 TOML；根表和所有固定子表拒绝未知字段，重复键、错误类型、未知枚举均拒绝。不存在隐式类型转换。
- 两个文件的 `schema_version` 都是必填的无符号 32 位整数，只接受 `1`，缺失及其它值均报错。软件版本与 schema 版本独立。
- 可选值省略时为无值；列表默认为 `[]`，映射默认为空，除下表另有规定。TOML 不使用 `null`。序列化不保留注释、排版、可选空表的原貌；往返保证解析后的数据语义。
- manifest 的 `workspaces`、`target`、`env` 和 `target.<platform>.replace` 是开放键映射，值仍须满足规定类型；不是任意扩展字段入口。
- 文件内平台名称仅接受 `linux-x86_64` / `linux-aarch64`。CLI 的 `amd64`、`arm64` 等简写不能写入文件。Conda 子目录名称 `linux-64` / `linux-aarch64` 与文件平台枚举不同。
- 相对路径以清单所在目录为基准；源码 `name` 是 workspace 下 `src/` 内的仓库路径。路径字段不在解析时要求文件存在。显式 Conda 锁中的包 URL 与摘要由 Conda 锁校验器另行检查。

## Manifest 字段全集

表中 `?` 表示可省略；未标注的字段必填。`字符串[]` 表示字符串列表；子表是否可省略单独列出。

| 表 / 字段 | 类型及默认语义 |
| --- | --- |
| 根 | `schema_version` 整数；`project`、`environment`、`workspaces` 必填；`build`、`dependencies`、`target`、`env`、`editor`、`export` 可省略 |
| `project` | `name` 字符串，非空白且不含控制字符；`description?` 字符串；`ros_distro` 为 `humble / jazzy / rolling`；`platforms` 平台列表，至少一项 |
| `environment` | `provider` 为 `native / pixi / docker`；`native?`、`pixi?`、`docker?` 子表，只允许与 provider 相符的表；native 可省略，另外两者必须有对应子表 |
| `environment.native` | `ubuntu?` 字符串，未给出时 Humble 为 `22.04`、Jazzy / Rolling 为 `24.04`；`ros_base?` 字符串，默认 `ros-<distro>-ros-base` |
| `environment.pixi` | `manifest?` 路径，默认 `pixi.toml` |
| `environment.docker` | `base_image` 字符串；`dockerfile?` 路径；`platforms?` 字符串[]，buildx 名称 |
| `build` | `verify?` 字符串[]，每项非空白且不含 NUL；`conda?` 子表 |
| `build.conda` | `name?` 字符串、`prefix?` 路径、`managed?` 子表三选一，仅 native；名称与路径的限制见 spec §2.2 |
| `build.conda.managed` | `python` 数字字符串 `major.minor[.patch]`；`channels` 字符串[]，至少一个非 `nodefaults` 项，不允许空白、前后空白、前导 `-`、NUL 或换行；`environment_file?`、`lock_file?` 路径，非空白且无 NUL。锁路径默认 `conda-{platform}.lock`，多平台的自定义路径必须含 `{platform}` |
| `workspaces.<name>` | `path` 路径；`sources?` 路径；`underlays?` 字符串[]；`colcon?` 子表。至少一个 workspace；underlay 必须存在，不能自引用或形成环；拓扑排序的并列项按声明顺序处理 |
| `workspaces.<name>.colcon` | `defaults?` 路径；`mixins?`、`build_args?` 字符串[] |
| `dependencies` | `apt?`、`pip?` 字符串[]；`rosdep?` 子表。pip 仅记录，不由 apply 安装 |
| `dependencies.rosdep` | `enabled?` 布尔，默认 **true**；`skip_keys?`、`extra_sources?` 字符串[] |
| `target.<platform>` | 平台必须列于 project；`dependencies?`、`exclude?` 子表，各仅含 `apt?` 字符串[]；`replace?` 为包名到包名的字符串映射 |
| `env` | 环境变量名到字符串的映射；名称为 `[A-Za-z_][A-Za-z0-9_]*`，禁止 `AMENT_PREFIX_PATH` / `COLCON_PREFIX_PATH` |
| `editor` | `vscode?`、`clion?`、`devcontainer?` 布尔，均默认 false |
| `export` | `compression?` 为 `none / gzip / zstd`，默认 none；`sign?` 布尔，默认 false |

依赖与 target 中的所有 apt 包名（含替换两端）符合 `[a-z0-9][a-z0-9+.-]*`。覆盖按追加、替换、排除、去重执行，详见 spec §2.2。`native.ubuntu` 等字符串的运行平台适配检查发生在 provider 执行阶段，而不是 TOML 类型校验阶段。

Pixi / Docker、editor / export 字段是已存在的格式预留，冻结它们的字段形状不代表这些功能可执行；支持状态仍以 README 与 phases 为准。

## Lock 字段全集

| 表 / 字段 | 类型及默认语义 |
| --- | --- |
| 根 | 必填 `schema_version` 整数、`generated_at` 字符串、`manifest_sha256` 字符串、`provider` 表；`sources?`、`apt?`、`pip?`、`conda?` 均为表数组，默认空 |
| `provider` | `kind` 为 `native / pixi / docker`；`native?`、`pixi?`、`docker?` 子表 |
| `provider.native` | `ubuntu` 字符串；`ros_snapshot?` 日期字符串 `YYYY-MM-DD`；`ubuntu_snapshot?` 时间字符串 `YYYYMMDDTHHMMSSZ`；缺快照表示实时源、可重放能力降级 |
| `provider.pixi` | `manifest`、`lock`、`lock_sha256` 字符串；只引用外部锁，不复制包列表（预留） |
| `provider.docker` | `base_image`、`digest` 字符串（预留） |
| `sources[]` | `workspace`、`name`、`type`、`url`、`rev` 字符串；`version?` 字符串。生成的 git `rev` 为精确提交，`version` 仅保留请求的分支 / tag 供阅读 |
| `apt[]` | `platform` 平台枚举；`name`、`version` 字符串；`sha256?` 字符串 |
| `pip[]` | `name`、`version` 字符串；只供 verify 对比 |
| `conda[]` | `platform` 平台枚举；`lock`、`lock_sha256`、`definition_sha256` 字符串；每平台一个记录，引用该平台显式锁 |

解析阶段额外拒绝非法 apt 包名、空或含 `[A-Za-z0-9.+~:-]` 之外字符的 apt 版本、空或非十六进制的源码 rev。其它字符串不在 `Lock::parse` 中作格式认证（包括时间、摘要、rev 长度）；文件能解析不等于可重放。provider 与清单是否匹配、源码路径、底包是否存在、Conda 记录与摘要是否匹配等由 apply / verify 按 spec §3 检查。生成器使用完整 git 提交 SHA、RFC 3339 UTC 时间、`sha256:` 加 64 位小写十六进制摘要，不应手工用占位值替代。

## 哈希与重放语义

- `manifest_sha256` 对 UTF-8 清单原文逐字节计算 SHA-256，不做 TOML 规范化；注释、换行和空格变化也使锁过期。`.repos`、Conda YAML 不拼入该哈希。
- git 按 `rev` 检出；apt 按平台、包名记录精确版本，可选 deb 摘要来自仓库索引。同版本重新锁定时可保留既有摘要；版本变化不得继承旧摘要。快照和降级、Ubuntu 已装更新版本的保留规则见 spec §3.2–3.4；apt 记录不是整个系统的传递依赖闭包。
- `lock` 解析本机平台，合并符合清单与共享快照约束的其它平台 apt / Conda 记录。两种架构共享 `provider.native` 快照字段，不能暗示每平台独立快照。apt 按平台（x86_64 在前）、包名排序；Conda 按平台排序；源码按 workspace 拓扑与 repos 顺序。仅生成时间不同不重写锁。
- 外部 Conda 的名称 / prefix 不锁包，不生成 `conda[]`。受管环境的 `lock_sha256` 是显式锁文件原文摘要，`definition_sha256` 是规范化 YAML 的摘要（移除输入 name / prefix，按清单处理 channels / Python 并添加 nodefaults）。具体构建由 `conda/definition.rs` 定义并由 Conda 测试覆盖；重放必须核对定义和显式锁摘要，不允许重新求解兜底。

## 验证入口

`cargo test -p rospm-core --test schema_v1` 固定完整字段往返、缺省值、原文哈希、版本 / 类型 / 平台拒绝及每层固定表的未知字段拒绝。现有 `tests/manifest.rs`、`tests/lock.rs`、`tests/lock_shared_snapshot.rs`、Conda 核心测试与 CLI 的 lock / apply_locked / verify 测试覆盖业务约束。fixtures 的 URL、提交及包摘要是合成数据，只供离线解析测试，不作为安装或真实快照验收证据。
