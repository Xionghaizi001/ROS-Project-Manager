# P4.5.2 worker 协议与 socket 验证

日期：2026-10-31。

`rospm-core::worker` 提供按需 worker 与 TUI 之间的最小本机传输约束。传输是 Unix stream 上的换行分隔 JSON，每帧带 `version = 1`；超过 1 MiB 的帧或未知版本直接拒绝。

## socket 与身份

- worker 目录由调用方按用户目录选择（建议 `$XDG_RUNTIME_DIR/rospm`），创建后强制 `0700`，必须由当前有效 UID 所有，目录本身不能是符号链接。
- socket 名为 `worker.sock`，权限强制为 `0600`。已有 socket 不会被删除或覆盖，避免第二个 worker 夺取任务；非 socket 路径也直接失败。
- Linux 接受连接后通过 `SO_PEERCRED` 检查对端 UID 必须等于当前有效 UID，同时保留 PID/GID 供审计。其他 Unix 平台应提供等价的本机凭据校验后再启用传输。

## 消息与幂等

消息只允许 `apply`、`apply_locked`、`build`、`uninstall`、`check`、`verify`、`deps` 等枚举操作，不接受 shell 字符串或任意提权命令。`Submit` 必须同时携带 `request_id` 和 `task_id`；worker 保存 request 到 task 的映射，重连重复提交同一 request 只返回原任务，不会再次执行，映射到其他任务则拒绝。

`Snapshot` 返回任务状态、步骤、总步骤、日志游标和最终结果；`Logs` 以单调游标请求增量，`Confirm` 必须绑定 task 与 step 并显式回复 `approve`、`reject` 或 `cancel`。任务状态包含等待确认、取消请求、已取消、失败、中断和待核实等终态，故障恢复不得据此自动重放有副作用操作。

当前实现是协议、身份和路径边界，任务日志文件、worker 生命周期及慢客户端队列由本阶段后续待办实现。现有 `.rospm/log/build.log` 仍是兼容性的覆盖日志，不作为任务历史。

## 断连与故障恢复

UI 或 SSH 客户端断开不会改变任务状态，worker 继续执行并保存快照，重连使用原 `task_id` 读取状态和日志。worker 退出或主机故障则由启动恢复流程把活动任务分别标记为 `interrupted` 或 `unknown`；恢复流程只写状态，不自动重新执行任何有副作用操作。
