# Phase 4 启动清单：Release 与 schema v1

更新日期：2026-10-25。当前为发布准备阶段，版本仍为 `0.0.1`；manifest / lock schema v1 已冻结，1.0 尚未发布。阶段总状态见 [phases.md](phases.md)。

## 已有基线与发布前缺口

| 项目 | 当前证据 | 下一步 |
| --- | --- | --- |
| native 与锁定重放 | [P1](p1-native-validation.md)、[P2](p2-locked-validation.md) | 补齐独立机器同锁比较与间隔复测，保留环境与锁摘要 |
| aarch64 | [QEMU 验收](p3-qemu-validation.md) | 原生 `ubuntu-24.04-arm` runner 复跑同一 fixture 的还原、构建与节点运行 |
| Conda | [已有环境验收](p35-existing-conda-validation.md)、[干净环境一键验收](p35-managed-conda-validation.md) | 纳入发布回归；原生 ARM 仍按 Phase 3 门槛复测 |
| 常见错误诊断 | [P3.6 验收](p36-diagnostics-validation.md) | 纳入发布回归，保持原始日志、失败状态和 JSON 契约 |
| CI | 已配置 lint、双架构 test/build 与容器 smoke | 在 GitHub 实际跑通并保留链接；补充新容器锁定还原场景 |
| 分发 | Cargo workspace 版本 `0.0.1`，CI 上传构建产物 | 制定版本、打包、摘要与发布流程，验证运行时兼容性 |

尤其需要验证现有 CI 在 Ubuntu 24.04 构建的二进制能否运行于 22.04；不能只凭编译成功声明兼容 Humble。若 glibc 基线不兼容，应调整构建环境或分别提供产物，并实测。

## 建议实施顺序

### 1. 补齐验收与回归基线

- [x] 为 Phase 3.5 干净机一键还原建立可复跑脚本与验收记录，覆盖两种架构的安装器选择、重复复用、中断重试、摘要错误、缺锁记录、shell 启动文件不变，以及另一干净环境的包记录一致性。
- [ ] 完成原生 ARM fixture 验收及 GitHub CI 实跑，记录 commit、平台、命令、退出状态与日志。
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

当前没有实际用户，不安排冻结前迁移与向后兼容工作；格式冻结与原生 ARM / 发布产物验收分开，未完成的发布门槛仍保留。

### 3. 发布工程与候选版本验证

- [ ] 确定 x86_64 / aarch64 产物命名、Ubuntu / glibc 最低运行要求及安装说明。
- [ ] 增加受控的 tag 发布流程，打包二进制、使用说明、许可证和 SHA256 摘要；核对仓库实际许可证文件与 Cargo 声明一致。
- [ ] 整理 changelog、支持矩阵与已知限制；明确 Pixi / Docker、离线、TUI / GUI 不属于此次稳定基线。
- [ ] 对候选产物在声明支持的平台执行 `--version`、帮助、预览、真实锁定还原、构建及验证，保存证据。
- [ ] 所有发布门槛通过后更新版本与锁文件，发布 1.0，并记录 tag、commit、产物摘要和验收链接。

## 本次准备结果

- [x] 中文 README 与当前能力、验收边界对齐。
- [x] 建立 Phase 4 验收缺口、schema 审计与发布步骤清单。
- [x] 规范、README 与阶段清单同步为 schema v1 已冻结、1.0 尚未发布。

Phase 3.5 干净 Conda 环境一键验收已完成；下一项优先工作是原生 ARM 与 GitHub CI 实跑。schema v1 已独立完成冻结，1.0 发布仍须关闭其余发布验收缺口。
