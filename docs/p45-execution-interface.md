# P4.5.1：执行接口与共享确认规则

日期：2026-10-29。覆盖 4.5.1 的前三个复选项；worker 与进程组信号执行仍是后续待办。

## 终端读写盘点

| 位置 | 当前读写与用途 | 接口边界 |
| --- | --- | --- |
| CLI `commands/apply.rs` | 换源探测进度写 stdout；确认写 stderr、读 stdin；检查 TTY，EOF 拒绝 | 探测与报告仍在 CLI；自动换源确认由 core `confirmation::switch_mirror` 处理 |
| CLI `commands/uninstall.rs` | 审阅计划、TTY / `--yes` 检查、卸载确认 | core `confirmation::uninstall` 绑定审阅的命令、包名与系统文件操作；CLI 提供终端适配器 |
| CLI `commands/build.rs` | 计划输出；失败后列出 build / install 路径、读确认后清理重试 | core `confirmation::retry_build` 负责失败资格、目录范围、确认、清理与一次重试 |
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
- `execution::Confirmation` 携带任务、步骤、带类型的操作范围、影响说明与 `Message` 提示；`Interaction::confirm` 返回 `Result<bool>`，默认 `Ok(false)`。core 共享确认流程已接入 CLI。无客户端等待与重连属于后续 worker，不将断连或超时当作同意。
- `diagnostic::summary::{collect, Summary, Group, DiagnosticStatus}` 从 CLI 移入 core，继续调用 Phase 3.6 的 C++ / Python 解析器，保持 JSON 字段和过期日志过滤。结构化入口在构建命令失败时发送同一 Summary；CLI 继续负责终端排版。诊断结果不改变命令成功 / 失败。

## 共享确认流程（第二项）

`rospm-core::confirmation` 提供三条业务入口。CLI 的 `commands/interaction.rs` 仅处理 TTY、显示、flush 和读输入；TUI 可实现同一个 `Interaction`，执行闭包调用 `Plan::perform_with`。

- `switch_mirror`：只用于自动推荐镜像，明确镜像名称、实际 ROS apt / rosdistro URL；锁定快照时不将 ROS apt 列为更换范围，apt-only 镜像如 Aliyun 的 rosdistro 仍是官方地址。显式 `--mirror` / `ROSPM_MIRROR` 沿用已有探测和选择规则。终端读到空行仍默认同意，EOF 与读取错误不算空行。
- `uninstall`：范围包含该计划的完整 `Command` 数据（包名、参数、提权、环境与工作目录）。共享函数将同一不可变 Plan 交给执行闭包，拒绝或确认错误时不调用它；没有实际操作时不确认、不执行。只有显式传入的 `yes`（CLI `--yes`）跳过确认。当前 native 卸载只包含 Run / Note，出现未支持的操作时拒绝执行，避免审阅范围漏项。
- `retry_build`：只对原有的 Conda stale-cache / 当前计划中失败的 colcon build 提供清理；选择与原 CLI 相同的工作区和 build / install base。先列出完整绝对目录、删除整个目录（包括本次未选包）的影响和重试命令；拒绝时返回 false，让调用者保留原错误。通过后清理并调用同一计划的执行闭包一次，第二次失败不会继续自动循环。

确认的 `task` 由调用方分配；CLI 使用进程号、时间与序号生成本次操作标识。卸载的 `step` 是第一条实际命令位置，清理的是失败步骤位置，换源发生在生成计划前，使用 `step = 0`。回复是对当前借用请求的同步回答，不生成可缓存、复用到另一计划的许可。后续 socket 适配器必须匹配任务、步骤和完整操作范围，不能用其他请求的回复完成当前调用。

清理前拒绝项目/工作区根目录、源码目录及其父目录、作为清理根的符号链接；目录内的符号链接不会被递归跟随。重复和嵌套目录去重。确认后、首次删除前，检查所有已审阅目录的类型、真实路径、设备号和 inode；被替换时全部停止。删除失败会上报，不启动重试；已成功删除的目录不承诺回滚。此检查不提供对抗另一个进程在实际删除期间持续修改文件树的事务隔离，工作区互斥仍属于 worker 待办。

CLI 保留非终端换源提示显式指定镜像、非终端卸载需要 `--yes`、JSON / 非终端构建不弹出清理确认的规则。确认提示先完整写出并 flush，显示失败、EOF、输入错误、默认未实现或断开的适配器均不会授权。`--dry-run` 不进入上述执行/清理路径。

## 任务状态与取消契约（第三项）

机器状态由 `execution::State` 定义，序列化为 snake_case，不依赖翻译文本：

| 状态 | 含义与退出结果 |
| --- | --- |
| `running` | 正在准备或执行；无最终结果 |
| `waiting_confirmation` | 正在等待绑定当前操作范围的回复；无最终结果，断连不等于同意或取消 |
| `cancel_requested` | 已收到取消意图，尚不能据此认定子进程或副作用已停止；无最终结果 |
| `cancelled` | 已在安全边界停止，不再启动下一步；返回 `Err(Error::Cancelled)`，保留已完成的更改 |
| `succeeded` | 全部步骤完成，最后的取消检查也通过；返回 `Ok(())` |
| `failed` | 命令、日志、文件或交互发生错误；返回原始 `Err`，不被同时到达的取消请求覆盖 |

`Plan::perform_with` 每次发出一次 Task running 和一次最终 Task 事件。正常路径为 running → succeeded / failed；取消路径为 running → cancel_requested → cancelled。最终结果不可被迟到请求改写；重试必须使用新任务标识。Task 的 error 在失败或取消时携带对应 Message，在成功时为 null。子进程非零退出（包括外部信号退出）仍是 CommandFailed，不能仅凭信号推断用户取消。

新增 `Event::Interaction { task, step, state }` 用于计划生成前或执行间的控制状态，不伪造步骤完成事件或未知的 total。共享确认入口发出 waiting_confirmation；正常答复后恢复 running，交互错误发出 failed，取消发出 cancelled。拒绝是一次有效答复，由原业务规则决定中止操作或保留原失败。确认流程本身不重复发出完整 Task 生命周期；将准备、确认与 Plan 组合成任务的 worker 必须依据返回的 Result 记录唯一最终结果（包括跳过确认的 `--yes` 路径）。

`Interaction::cancel_requested(task)` 默认为 false；适配器按 task 保存单调置位的取消标记，查询必须快速返回。共享执行器在开始前及每个完整步骤成功后检查，包含空计划开始和最后一步结束。取消只在检查点被执行器观测并发出 cancel_requested；请求到达时的即时 UI 状态由持有标记的 worker 提供。本阶段不轮询运行中的子进程。检查通过后到下一步开始之间到达的请求，可能延后到该步完成才生效。终态之后到达的请求不再检查。

确认前、正常答复后检查取消；等待中的取消优先于同意或拒绝。阻塞的 socket 适配器必须在取消时唤醒 confirm 回调，返回答复或 Error::Cancelled；同步 core 无法强行唤醒任意阻塞回调。交互 I/O 错误仍保留为失败。默认拒绝、EOF、断连和显式取消是不同情况。显式 `--yes` 仅省略确认，不能绕过取消检查。

清理重试在确认后、全部目录复核后和全部删除完成后检查取消；删除这一组目录期间不插入取消点，删除错误照常保留，完成后若取消则不重试。目录已删除不回滚。Plan 中的 Conda 步骤（含内部多条安装命令）、FetchKey、文件写入和普通命令都视作完整步骤；apt / dpkg / Conda 安装进行中不主动发信号，不承诺即时取消、超时强杀、包数据库恢复或事务回滚。

### 进程组边界

当前接口不调用 kill、不创建独立进程组，也不拦截 CLI Ctrl-C；不能向继承的进程组发取消信号，否则可能伤及调用 shell 或其他任务。后续 worker 若支持可中断的构建 / 查询步骤，必须先为该任务子进程建立并验证独立进程组，只向它拥有且仍在跟踪的组发送信号，排除 worker、TUI、登录 shell 和其他任务。不得使用按名称杀进程或扫描用户进程作为取消实现；PID / PGID 生命周期校验、组内后代回收及终端归属须在 4.5.2 / 4.5.4 实测。

后代自行 setsid / 脱离组或提权后无法信号控制时，不承诺“杀掉进程树”；未确认停止不能记为 cancelled。对已审定可中断的步骤，未来 worker 应先请求温和退出并回收子进程、排空日志，再发布最终结果；不得默认将安装步骤纳入 SIGKILL 升级策略。UI 退出、网络断连和取消请求分开；worker / 主机异常中断也不能伪报 cancelled 或自动重放副作用。

现有 CLI 未接入主动取消按钮或信号转换，原有成功 0、错误 1 和 shell 信号行为保持不变。若共享 Error::Cancelled 经现有 CLI 错误路径传播，退出仍为 1；结构化调用方用枚举区分 failed 与 cancelled，不引入新的 CLI 退出码或修改 JSON 文档格式。

## 兼容与边界

现有 manifest / lock schema v1、CLI 参数、dry-run 预览和 JSON 文档格式不变。确认界面补充实际源 URL、完整目录和重试命令，中英文继续使用 i18n。结构化调用者先读取 / 展示 Plan；预览时不要调用 `perform_with`。此入口执行时关闭子进程 stdin，rospm 生成的 sudo 使用 `-n`，没有有效授权就失败，不向前端终端索要密码；CLI 原入口继续支持交互认证。任意用户命令仍可能自行打开 `/dev/tty`，此接口不提供沙箱或进程会话隔离。

此处没有 worker、socket、进程组信号中断、任务日志历史或客户端队列。回调若同步阻塞仍会拖慢读取；后续 worker 必须将持久化和 UI 发送解耦。`.rospm/log/build.log` 仍按原方式覆盖，不是任务历史。已有 `Command.log` / `Plan.log` 的落盘格式保持一致。

## 验证

- `cargo test --workspace`：原有 CLI / core 回归，包括中英文、JSON、dry-run、Conda fixtures 与构建诊断。
- 新增执行接口测试：大于管道容量的 stdout / stderr 同时输出、非 UTF-8 字节、stdin EOF、空计划、步骤失败停止、启动 / 写文件失败、诊断复用、确认默认拒绝。
- core 单元测试补充：`/dev/full` 日志写入失败后排空双流、sudo 非交互参数（含 preamble）、Conda 使用共享捕获并保留失败。
- 共享确认测试：任务/步骤/范围、镜像空行与 EOF 区别、显式 `--yes`、拒绝/断连不执行、选中工作区清理、源码与符号链接保护、目录替换检测、嵌套目录去重、一次重试及失败传播；CLI 适配器测试输入断连和显示写入失败。
- 取消测试：执行前（含空计划）、任务标识隔离、执行中请求后完成当前步骤并排空日志、停止后续步骤、真实失败优先、确认时取消覆盖同意、`--yes` 不绕过取消、取消清理不删除目录或重试。
- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`。

本次未执行真实 apt / sudo 安装、联网密钥验签、SSH 断连或 ARM 实机验收；相应集成边界不由这些单元测试证明。
