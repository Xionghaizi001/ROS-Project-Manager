# Phase 4 启动清单：Release 与 schema v1

更新日期：2026-10-27。当前为发布准备阶段，版本仍为 `0.0.1`；manifest / lock schema v1 已冻结，1.0 尚未发布。阶段总状态见 [phases.md](phases.md)。

## 已有基线与发布前缺口

| 项目 | 当前证据 | 下一步 |
| --- | --- | --- |
| native 与锁定重放 | [P1](p1-native-validation.md)、[P2](p2-locked-validation.md) | 补齐独立机器同锁比较与间隔复测，保留环境与锁摘要 |
| aarch64 | [QEMU 验收](p3-qemu-validation.md)、原生 ARM runner 的 workspace 测试与 release 构建已通过 | 开发阶段仅做预设支持；实机锁定还原、构建与节点运行由用户在发布后验收，不作为发布门槛 |
| Conda | [已有环境验收](p35-existing-conda-validation.md)、[干净环境一键验收](p35-managed-conda-validation.md) | 纳入发布回归；原生 ARM 实机验收按 Phase 3 留待发布后由用户完成 |
| 常见错误诊断 | [P3.6 验收](p36-diagnostics-validation.md) | 纳入发布回归，保持原始日志、失败状态和 JSON 契约 |
| CI | 2026-10-27 用户确认 GitHub lint、双架构 test/build 与 Ubuntu 22.04 / 24.04 容器 smoke 已通过 | 补充运行链接；补充新容器锁定还原场景 |
| 分发 | Cargo workspace 版本 `0.0.1`，CI 上传构建产物 | 制定版本、打包、摘要与发布流程，验证运行时兼容性 |

当前 CI 的 x86_64 构建产物已通过 Ubuntu 22.04 / 24.04 裸容器 smoke。候选发布产物仍需按声明的 Ubuntu / glibc 基线复核；该结果不代表原生 ARM 实机或硬件驱动已通过验收。

## 建议实施顺序

### 1. 补齐验收与回归基线

- [x] 为 Phase 3.5 干净机一键还原建立可复跑脚本与验收记录，覆盖两种架构的安装器选择、重复复用、中断重试、摘要错误、缺锁记录、shell 启动文件不变，以及另一干净环境的包记录一致性。
- [x] GitHub CI 实跑通过（2026-10-27，用户确认），覆盖 lint、双架构测试与构建、双系统容器 smoke；运行链接待补充。
- [x] 明确 ARM 支持边界：开发阶段仅做预设支持，原生 ARM fixture 实机验收由用户在发布后完成，不阻塞 1.0 发布。
- [ ] 明确 `env --shell fish` 等历史未完成项在 1.0 中的支持边界；不在帮助或文档中承诺未实现能力。
- [ ] 整理 native、已有 Conda、受管 Conda 示例，确保引用文件齐全，预览不写文件，实际还原有证据。

本地回归命令（不能替代真实机器验收）：

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p rospm-cli
```

### 2. 审计并冻结 schema v1

- [x] 逐项核对 manifest / lock 的字段、类型、默认值、未知字段处理、路径基准、平台名称与校验边界，见 [schema v1](schema-v1.md)。
- [x] 审计清单原文哈希、源码提交、apt 快照及版本保留、外部 / 受管 Conda、显式锁文件与摘要语义。
- [x] 保存最小与完整 native manifest、多平台 lock fixtures；契约测试覆盖解析与序列化往返、默认值、非法版本 / 类型 / 平台、必填项与逐层未知字段拒绝，含外部 Conda 与预留 provider 表。
- [x] 确认文档与实现一致，2026-10-25 冻结；冻结基线由引入 `docs/schema-v1.md` 的 Git 提交定位。`schema_version = 1` 与软件版本独立管理。

当前没有实际用户，不安排冻结前迁移与向后兼容工作；格式冻结与发布产物验收分开，其余未完成的发布门槛仍保留；原生 ARM 实机验收为发布后用户跟踪项。

### 3. 发布工程与候选版本验证

- [x] 确定发布包架构命名：x86_64 使用 `*.x86.*`，aarch64 使用 `*.arm64.*`，具体示例见下文。
- [ ] 验证 Ubuntu / glibc 最低运行要求并补齐安装说明：仓库预编译包以 Ubuntu 22.04 为最低支持目标；Ubuntu 20.04 的程序运行兼容性单独验证。
- [x] 增加由 `changelog.md` 推送触发的发布流程，解析版本与说明，通过检查后创建 tag、上传双架构发布包与 SHA256 摘要并公开 Release。
- [x] 已提供根目录 `LICENSE`（AGPL v3），Cargo 声明为 `AGPL-3.0-only`；发布包必须包含该文件，缺失时打包失败。
- [ ] 编写首版 changelog，以功能介绍和使用方法为主，不罗列开发期间的增量变更；整理支持矩阵与已知限制，明确 Pixi / Docker、离线、TUI / GUI 不属于此次稳定基线。
- [ ] 对 x86_64 候选产物在声明支持的 Ubuntu 版本执行 `--version`、帮助、预览、真实锁定还原、构建及验证，保存证据；aarch64 保留构建与开发阶段验证证据，实机闭环由用户在发布后验收。
- [ ] 所有发布门槛通过后更新版本与锁文件，发布 1.0，并记录 tag、commit、产物摘要和验收链接。

#### 产物命名与运行基线

| 架构 | 发布包命名 | 1.0 示例 |
| --- | --- | --- |
| x86_64 | `*.x86.*` | `rospm_1.0.0.x86.tar.gz` |
| aarch64 | `*.arm64.*` | `rospm_1.0.0.arm64.tar.gz` |

归档内的可执行文件统一为 `rospm`；清单与锁文件继续使用 schema v1 的 `linux-x86_64` / `linux-aarch64` 平台名称。

- **仓库预编译包**：最低支持目标为 Ubuntu 22.04（glibc 2.35），覆盖 22.04 / 24.04。当前 CI 在 Ubuntu 24.04 上构建，不能仅凭构建系统版本推断产物最低要求；发布前须检查两种架构产物所需的 glibc 符号版本及其他动态库依赖，确保不超出基线，并保留前述架构验收边界。
- **Ubuntu 20.04**：可作为程序本身的源码构建与运行兼容目标（glibc 2.31），但尚未验证，不能宣称已支持，也不承诺仓库预编译包可在该系统运行。确认支持前须在该系统构建并验证 `--version`、帮助等基本命令。
- **ROS 工作流**：程序能够启动不等于能还原 ROS 环境。当前实现只映射 Ubuntu 22.04 / 24.04，支持 Humble / Jazzy / Rolling，未实现 Ubuntu 20.04 / Foxy 的 native 工作流；不能把程序运行兼容目标写成 ROS 工作流支持承诺。

#### 自动发布用法

[Release workflow](../.github/workflows/release.yml) 仅在推送到 `main` 且根目录 `changelog.md` 发生变更时运行。普通代码推送、其他分支和 PR 不触发发布。

准备发布时，同步修改 `Cargo.toml` 的 workspace 版本及 `Cargo.lock` 中的工作区包版本，再创建或更新根目录 `changelog.md`。该文件仅保存本次发布内容，历史说明保留在 GitHub Releases；格式如下：

````markdown
# v1.0.0 首次发布

## 功能介绍

用清单描述、锁定并还原 ROS 2 项目环境。

## 使用方法

```sh
rospm init --distro humble --base ros-base
rospm check
rospm apply --dry-run
rospm apply --build
```
````

首个非空行必须为 `# vX.Y.Z`，可在版本后附标题，仅接受正式版本号；去掉 `# ` 后作为 Release 标题，版本生成 `vX.Y.Z` tag，其余正文原样作为更新说明。版本与 Cargo 不一致、正文为空或 tag 已存在都会阻止发布，不覆盖历史版本。

流程在原生 x86_64 / ARM runner 的 Ubuntu 22.04 容器内执行 workspace 测试、release 构建及基本 smoke，并在 x86_64 执行 fmt / clippy。随后打包 `rospm`、README、changelog、docs、examples 及 `LICENSE`，生成 `SHA256SUMS`。全部成功后，以触发推送的 commit 创建草稿 Release，上传两个架构的压缩包和摘要，最后公开；无需手动推 tag。该流程不替代前述真实 ROS 环境与硬件验收。

仓库须允许 GitHub Actions 使用 `GITHUB_TOKEN` 创建 Release（发布 job 申请 `contents: write`，其他 job 仅有读权限）。如果上传或公开失败，可能留下草稿或 tag；检查并清理该次未完成发布后才能重跑，已公开版本应通过新版本更新。当前未创建 `changelog.md`，避免尚未准备好的版本在下次普通推送时发布。

#### 首版 changelog 内容

这是第一个发布版本，changelog 直接面向新用户介绍功能和使用方法：

1. **功能介绍**：清单描述、依赖检查与安装、源码导入、锁定还原、构建与验证、环境加载、已有及受管 Conda 环境、错误诊断、中英文与 JSON 输出。
2. **安装与入门**：按架构选择上述发布包，校验 SHA256，解压并将 `rospm` 放入 PATH；引用 [README 快速开始](../README.md#快速开始)，说明 `init` → `check` → `apply --dry-run` → `apply --build` → `lock` → `verify`，以及另一台匹配机器上的 `apply --locked --build`。
3. **支持范围与限制**：列明预编译包最低 Ubuntu 版本、ROS 发行版、ARM 验收边界和未实现能力，链接现有说明；不把内部阶段编号、重构或开发提交列表作为首版更新内容。

## 本次准备结果

- [x] 中文 README 与当前能力、验收边界对齐。
- [x] 建立 Phase 4 验收缺口、schema 审计与发布步骤清单。
- [x] 规范、README 与阶段清单同步为 schema v1 已冻结、1.0 尚未发布。

Phase 3 开发阶段预设支持、Phase 3.5 干净 Conda 环境一键验收、GitHub CI 与 schema v1 冻结已完成。下一步整理发布回归、文档与示例，完成候选产物验证及发布工程；原生 ARM 实机验收由用户在发布后完成，不阻塞 1.0 发布。
