# P4.5.2 worker 协调与生命周期

日期：2026-11-01。

`rospm_core::worker::WorkspaceLock` 为规范化后的 workspace 路径生成用户私有运行目录中的锁文件，并用内核 `flock(LOCK_EX)` 持有整个任务生命周期。第二个 worker 会等待同一工作区的租约，或通过 `try_acquire` 得到“忙”并将任务保持为 pending；锁文件不依赖删除来表示所有权，也不会因 worker 崩溃留下无法清除的 stale lock。租约覆盖等待确认阶段，因此暂停任务时仍不会有第二个任务修改工作区。

native 计划中的 `apt-get`、`dpkg` 和相关命令继续使用它们自己的系统锁（例如 `/var/lib/dpkg/lock-frontend`、`/var/lib/dpkg/lock` 及 apt 的 lists/cache 锁）。worker 不删除、替换或伪造这些锁；workspace 租约只解决 rospm 任务之间的冲突，系统锁仍由 apt/dpkg 获取和等待。

`ConfirmationWait` 是一次任务范围的 rendezvous。客户端断开、TUI 退出或 SSH 会话消失都不会产生响应，worker 安全地停在 `waiting_confirmation`，直到客户端重连后发送 `approve` / `reject`，或明确发送 `cancel`。界面退出和取消是两个动作：只有取消请求会在安全步骤边界转为 `cancel_requested` / `cancelled`；安装等不可任意中断的步骤仍遵循执行接口的取消契约。
