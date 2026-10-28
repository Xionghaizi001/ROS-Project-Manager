# P4.5.1 首项：执行与交互接口基线

日期：2026-10-28。范围为 4.5.1 的第一个复选项；确认业务迁移、取消状态机和 worker 仍是后续待办。

## 终端读写盘点

| 位置 | 当前读写与用途 | 接口边界 |
| --- | --- | --- |
| CLI `commands/apply.rs` | 换源探测进度写 stdout；确认写 stderr、读 stdin；检查 TTY，EOF 拒绝 | 业务选择仍在 CLI；下一项迁移到共享确认规则 |
| CLI `commands/uninstall.rs` | 审阅计划、TTY / `--yes` 检查、卸载确认 | 沿用原规则；下一项绑定具体操作范围 |
| CLI `commands/build.rs` | 计划输出；失败后列出 build / install 路径、读确认后清理重试 | 沿用原规则；下一项迁移清理确认和重试 |
| CLI `commands/deps.rs` | 安装走 Plan；查询输出表格或 JSON | 安装复用同一执行器；查询仍是报告展示 |
| CLI `check` / `verify` / `doctor` / `init` / `lock` / `report` | stdout 表格、说明或 JSON | 已有结构化报告是数据源，终端输出只负责展示 |
| CLI `commands/environment.rs` | `env` 打印脚本；`shell` 继承终端执行 Bash | 前台 shell 交接属于 4.5.3，不能当普通后台计划运行 |
| CLI `main.rs`、`build_summary.rs` | 错误 / 诊断写 stderr；最终 JSON 写 stdout；JSON 模式用 fd 重定向隔离子进程输出 | 保持原格式、退出处理与 JSON 数据流；诊断收集已移入 core |
| core `plan.rs` | Plan 预览和进度写传入的 `Write`；普通命令继承终端；构建命令双流 tee 到终端与日志；curl / sudo 安装密钥、sudo 文件写入 | CLI 保留原入口；结构化入口捕获 stdout / stderr 与进度，不主动写前端终端；系统辅助命令也接入捕获 |
| core `conda.rs`、`conda/bootstrap.rs`、`conda/system.rs` | 检查 / 安装进度写 `Write`；run 子进程原先继承 stdout / stderr；query 捕获输出；download 写文件 | 结构化入口捕获进度与 run 子进程；保留现有校验、下载和测试替身，复用 Plan 的子进程执行函数 |
| core `provider/native/*`、`probe.rs`、`repository.rs`、`snapshot.rs`、`scm.rs` | provider 产出 Plan / 报告；探测命令通过 `output` 捕获或将流置空；部分命令 stdin 用于查询输入 | 不根据显示文本判定任务成败；没有新增 provider 执行副本。只读探测不作为安装步骤进度 |

## 可复用接口

- `Plan::execute(dry_run, out)` / `perform(out)` 保持调用方式；apply、build、deps install、uninstall 及重试仍走同一 `perform_inner`。
- `Plan::perform_with(task, interaction)` 执行已有 Plan，产生 `execution::Event`：任务 running / succeeded / failed、每步 running / succeeded / failed、日志及构建诊断。空计划成功；任一步失败后停止，后续步骤不执行。原始 `Result<()>` 仍返回调用者。
- `task` 由调用者分配；`step` 为 `Plan::steps` 中从 1 开始的序号，包含 Note；`total` 是全部步骤数。一个重试需要调用者分配新任务标识。机器字段和 snake_case 枚举值不随语言变化，不依赖解析进度文本。
- 日志按 `stdout` / `stderr` / `progress` 分流。stdout / stderr 是原始字节块，允许非 UTF-8 和跨行块，保留每个流内的顺序，不承诺两个流间的全局顺序。progress 是现有 `Message` 渲染后的显示文本，不是状态数据。中英文继续用已有 i18n key，无新增硬编码用户提示。
- `Interaction::event` 可能由两个管道读取线程并发调用，必须及时返回、不 panic。执行器只用固定 8 KiB 缓冲，不在内存积攒整个日志；写盘错误在继续排空管道并回收子进程后上报。终端写入失败不阻止日志保存。
- `execution::Confirmation` 提供任务、步骤、操作类型、路径和 `Message` 提示；`Interaction::confirm` 默认拒绝。该接口暂不代替 CLI 的换源 / 卸载 / 清理规则；绑定完整操作范围和断连等待属于下一项，不能将默认拒绝当作用户已确认。
- `diagnostic::summary::{collect, Summary, Group, DiagnosticStatus}` 从 CLI 移入 core，继续调用 Phase 3.6 的 C++ / Python 解析器，保持 JSON 字段和过期日志过滤。结构化入口在构建命令失败时发送同一 Summary；CLI 继续负责终端排版。诊断结果不改变命令成功 / 失败。

## 兼容与边界

现有 manifest / lock schema v1、CLI 参数、dry-run 预览、中文 / 英文输出及 JSON 文档格式不变。结构化调用者先读取 / 展示 Plan；预览时不要调用 `perform_with`。此入口执行时关闭子进程 stdin，rospm 生成的 sudo 使用 `-n`，没有有效授权就失败，不向前端终端索要密码；CLI 原入口继续支持交互认证。任意用户命令仍可能自行打开 `/dev/tty`，此接口不提供沙箱或进程会话隔离。

此处没有 worker、socket、取消、等待确认状态机、任务日志历史或客户端队列。回调若同步阻塞仍会拖慢读取；后续 worker 必须将持久化和 UI 发送解耦。`.rospm/log/build.log` 仍按原方式覆盖，不是任务历史。已有 `Command.log` / `Plan.log` 的落盘格式保持一致。

## 验证

- `cargo test --workspace`：原有 CLI / core 回归，包括中英文、JSON、dry-run、Conda fixtures 与构建诊断。
- 新增执行接口测试：大于管道容量的 stdout / stderr 同时输出、非 UTF-8 字节、stdin EOF、空计划、步骤失败停止、启动 / 写文件失败、诊断复用、确认默认拒绝。
- core 单元测试补充：`/dev/full` 日志写入失败后排空双流、sudo 非交互参数（含 preamble）、Conda 使用共享捕获并保留失败。
- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`。

本次未执行真实 apt / sudo 安装、联网密钥验签、SSH 断连或 ARM 实机验收；相应集成边界不由这些单元测试证明。
