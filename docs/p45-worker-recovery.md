# P4.5.2 worker 故障恢复边界

日期：2026-11-01。

任务执行与前端连接分离。TUI 断开只影响日志推送，SSH 连接断开也只影响该会话；worker 继续持有子进程、工作区租约和任务快照，客户端可用原任务标识重连读取快照与日志。断连不会触发取消、确认或重新提交。

worker 进程退出后，下一次启动可调用 `TaskStore::recover_active_tasks(RecoveryCause::WorkerExit)`。所有仍处于 `pending`、`running`、`waiting_confirmation` 或 `cancel_requested` 的任务会原子写成 `interrupted`，附带“先检查日志再重试”的结果说明。已结束任务保持原状态。

主机重启、掉电或无法证明子进程最终状态时使用 `RecoveryCause::HostFailure`，活动任务写成 `unknown`，结果说明要求先核对系统和日志。`recover_active_tasks` 只修改快照，不启动命令、不重放计划；`apply`、卸载、清理等有副作用步骤必须由用户审阅后显式重新提交。worker / 主机故障与 UI / SSH 断连因此不会被混为同一种恢复路径。
