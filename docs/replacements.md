# 内置 apt 替换映射表

Phase 3 的映射表位于 [`crates/rospm-core/data/apt-replacements.toml`](../crates/rospm-core/data/apt-replacements.toml)，独立于合并逻辑，编译时随 rospm 打包。`schema_version = 1` 表示表格式，`revision` 表示数据修订。更新 rospm 即可取得新版默认规则；运行时不联网更新规则，也不改写项目清单。

当前 `2026-10-19.1` 仅对 `linux-aarch64` 提供以下默认值；x86_64 没有默认替换：

| 原 apt 包名 | aarch64 目标包名 |
|---|---|
| `gcc-x86-64-linux-gnu` | `gcc-aarch64-linux-gnu` |
| `g++-x86-64-linux-gnu` | `g++-aarch64-linux-gnu` |
| `binutils-x86-64-linux-gnu` | `binutils-aarch64-linux-gnu` |
| `libc6-dev-amd64-cross` | `libc6-dev-arm64-cross` |

这些规则将显式声明的 GNU 目标工具链切换为 ARM 目标工具链，适用 Ubuntu 22.04 / 24.04；数据文件中保留 Ubuntu 包目录链接。它们不表示 rospm 已配置 CMake 交叉编译环境，也不保证项目源代码兼容 ARM。若项目在 ARM 主机上仍需要生成 x86 代码，请使用下面的同名覆盖保留原工具链。

CUDA 工具包版本和 Jetson 驱动取决于 NVIDIA 仓库、JetPack 与设备；MKL 到 OpenBLAS 也不是通用 ABI 等价替换，因此没有这类默认规则。项目应在 `target` 中声明已验证的替代、追加包或排除项；未映射的包保留原名，目标仓库检查会按已有逻辑报告 `missing` 或 `unknown`，不会静默删除。

合并规则：

1. 先取 `dependencies.apt`，追加 `target.<platform>.dependencies.apt`。
2. 按**原包名**查找用户 `replace`，没有用户规则才查内置表。只替换一次，不递归；即使清单没有对应 `target` 表，内置规则也生效。
3. 按替换后的包名执行用户 `exclude.apt`，最后保持原顺序去重。

例如：

```toml
[target.linux-aarch64.replace]
# 用户规则优先；可立即修正内置默认值，无需等待 rospm 发布。
"gcc-x86-64-linux-gnu" = "gcc"
# 同名覆盖禁用此项内置规则。
"g++-x86-64-linux-gnu" = "g++-x86-64-linux-gnu"

[target.linux-aarch64.exclude]
# 排除规则匹配替换后的名字。
apt = ["binutils-aarch64-linux-gnu"]
```

匹配是完整字符串相等；不会推断带版本（`pkg=version`）、显式架构（`pkg:amd64`）或其他拼写的替代。范围仅为额外 apt 依赖，包含目标追加项；不改写 ROS 底包、rosdep 键、`package.xml`、pip 或 conda 依赖。

`check`（含 `--json` 的 `notes`）、普通 `apply` / `--dry-run` 和 `lock` 会显示实际保留的替换、来源及内置表修订号。`check --target` 查询替换后的包名；新 `lock` 记录替换后包的本机版本。`apply --locked` 始终安装锁文件中的目标平台包名和版本，不用新版映射重新替换锁记录；需要采用新默认值时先检查普通 `apply --dry-run`，再执行 `apply` 并重新 `lock`。

维护默认表时，编辑独立 TOML，递增 `revision`，为新规则记录适用平台、Ubuntu 版本和包目录依据，并补充语义测试。只收录目标明确的规则；需要硬件、第三方仓库或源码适配的替代留给项目声明。运行 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace` 后随版本发布。数据格式、合法包名、循环 / 链式规则、用户优先级、安装、锁定重放和仓库查询均有回归检查。
