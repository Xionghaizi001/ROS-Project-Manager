# worker CLI 使用说明

`rospm worker` 在当前项目中启动一个前台任务服务，`rospm task` 通过 Unix socket 提交和管理任务。本文中的命令都针对包含标准 `rospm.toml` 的项目目录。需要从其他目录操作时，可以在命令前添加全局 `--manifest /path/to/rospm.toml`。

## 启动和提交任务

在项目目录启动 worker

```sh
rospm worker --runtime /tmp/rospm-my-project
```

在另一个终端提交任务并查看状态和日志

```sh
rospm task --runtime /tmp/rospm-my-project submit build --id build-001
rospm task --runtime /tmp/rospm-my-project status build-001
rospm task --runtime /tmp/rospm-my-project logs build-001 --cursor 0 --limit 100
rospm task --runtime /tmp/rospm-my-project cancel build-001
```

`worker` 和 `task` 使用同一个 `--runtime`，例如 `/tmp/rospm-my-project` 或 `$XDG_RUNTIME_DIR/rospm-my-project`。worker 绑定启动时的项目，只处理该项目的提交。

CLI 使用任务 ID 作为请求 ID。ID 长度为 1–128 个字符，可使用 ASCII 字母、数字、点、下划线和连字符，单独的 `.` 和 `..` 除外。提交成功返回 `accepted`，表示请求已接收。用 `status` 查看快照中的 `state`、当前 `step`、总步骤数 `total_steps` 和最终 `result`。`total_steps` 未知时为 `null`。所有 `task` 成功响应都是协议 v1 JSON，无需添加 `--json`。查询到失败任务不会让查询命令本身失败。

常用操作包括 `apply`、`apply_locked`、`build`、`uninstall`、`check`、`verify` 和 `deps`。可通过 `rospm task --runtime /tmp/rospm-my-project submit --help` 查看可选值。

- 使用清单默认配置和官方镜像。`build` 构建整个 workspace，不隐含执行 `build.verify`。
- `apply_locked` 和 `verify` 需要与清单匹配的锁文件。规划错误也会写入失败结果。
- `check`、`verify` 和 `deps` 将检查报告写入任务日志。环境不满足要求时，任务状态为失败。`deps` 只检查依赖，不执行安装。
- worker 不提供同步 CLI 的临时参数覆盖、镜像自动选择、自动清理重试和 `apply --build` 组合。需要这些选项时使用对应的同步命令。
- 系统命令使用非交互 `sudo -n`，socket 不接收密码。权限不足会直接记录为失败。

同一项目中的任务按顺序取得 workspace 租约，租约覆盖规划、确认等待和结果写入。最多保留 32 个活动或排队任务。排队任务可以直接取消。

## 确认和取消

卸载任务会停在 `waiting_confirmation`。先读取日志中的完整操作 JSON，再使用待确认步骤的实际编号回复

```sh
rospm task --runtime /tmp/rospm-my-project submit uninstall --id remove-001
rospm task --runtime /tmp/rospm-my-project logs remove-001
# 将 2 换成快照中待确认的 step
rospm task --runtime /tmp/rospm-my-project confirm remove-001 --step 2 reject
```

确认响应只能是 `approve`、`reject` 或 `cancel`。错误步骤、重复响应和已经取消的确认会被拒绝。客户端断开不会批准或取消任务。任务运行期间可以使用 `cancel` 请求取消，实际生效时间遵循执行步骤的取消边界。

## 断连、退出和恢复

`rospm task` 每次请求使用短连接。只要 worker 仍在运行，客户端退出或 SSH 断开后，活动任务仍由 worker 持有并继续执行，等待确认的任务继续等待。重新连接后使用原任务 ID 查询状态和日志即可。

worker 忽略 `SIGHUP`。收到 `SIGINT` 或 `SIGTERM` 后会请求取消活动任务，等待任务结束并删除 socket。可中断的 native 构建会向自己的进程组发送 `TERM`，超时后再发送 `KILL`。受保护步骤会等到安全边界，因此退出可能需要更长时间。worker 在前台运行，不会自动 daemon 化。需要离开终端时，可以显式创建新会话并重定向标准流

```sh
setsid rospm worker --runtime /tmp/rospm-my-project </dev/null >worker.out 2>&1 &
```

启动后始终没有客户端完成握手，或最后一个已握手的客户端断开时 worker 已空闲，worker 默认在 60 秒后退出。可以用 `--auto-exit-timeout` 设置空闲超时，单位为正整数秒。

```sh
rospm worker --runtime /tmp/rospm-my-project --auto-exit-timeout 120
```

最后一个已握手的客户端断开后，如果还有活动任务，worker 会在所有任务结束并保存结果后退出。排队和等待确认都属于活动状态。期间有客户端重新完成握手时，worker 会重新判定退出条件。SSH 登出后 worker 能否继续运行取决于主机的 session 和 cgroup 策略。

worker 停止后，在项目目录中用原来的 `--runtime` 运行 `status` 可离线查询终态，`logs` 可读取已保存的日志。上次故障留下的 `pending`、`running`、`waiting_confirmation` 和 `cancel_requested` 任务会在 CLI worker 重启时标为 `unknown`，已结束任务保持原状态。库接口 `TaskStore::recover_active_tasks` 还支持在调用方能确定任务已随 worker 退出而中断时使用 `RecoveryCause::WorkerExit`，将活动任务标为 `interrupted`。

恢复只更新快照。旧子进程可能仍在运行，等待中的确认需要随新任务重新处理。先核对日志、系统状态和旧子进程，再决定是否用新 ID 显式提交任务。

## 任务存储与提交去重

任务数据保存在项目的 `.rospm/worker/tasks/`，每个任务在 `<task-id>/` 下保存 `snapshot.json` 和 `logs.jsonl`。任务目录权限为 `0700`，两个文件权限为 `0600`。快照通过写入临时文件、同步并原子替换保存。worker 退出后保留任务数据，`--runtime` 只用于本机通信和运行时锁。同步命令的 `.rospm/log/build.log` 仍按原方式覆盖。

worker 在分派前持久化提交账本 `@submissions.json` 和初始 `pending` 快照，并使用 `@submissions.lock` 串行注册。CLI 提交响应丢失时，可用原任务 ID、项目和操作重试。直接使用协议时，重试还须原样保留 `request_id`、`task_id` 和 `workspace`。完全一致的请求只返回原 `accepted`，更改请求内容或占用已有任务 ID 会返回 `submission_conflict`。

每个请求最多分派一次。若账本已写入但未成功创建快照，重试会返回 `task_unavailable`。分派被拒绝会记为 `failed`，同一请求的重试不会再次入队。账本损坏会导致提交失败，worker 会保留原账本。

CLI worker 会保留已结束任务。库接口 `TaskStore::prune_completed` 可按数量清理最旧的终态任务目录，同时保留活动任务和去重账本。已清理任务的旧 ID 仍占用，重复提交返回 `task_unavailable`。

## 本机 socket 协议

`--runtime` 须由当前用户拥有，目录本身不得为符号链接。worker 将目录权限设为 `0700`，`worker.sock` 权限设为 `0600`。路径应足够短，以满足 Unix socket 的长度限制。一个项目和一个 runtime 各自只允许一个 worker 占用。CLI worker 取得两把锁后才会清理无法连接的残留 socket，遇到同名非 socket 路径会报错。Linux 上通过 `SO_PEERCRED` 校验对端的有效 UID 与 worker 一致。

协议在 Unix stream 上传输以换行分隔的 JSON，每帧带 `version = 1`，含换行最多 1 MiB。客户端先发送 `hello`，收到 `client` 为 `rospm-worker` 的 `hello` 响应后，再逐条发送请求并读取响应。服务最多保留 32 个连接，超出上限的新连接会被关闭。无效 JSON、未知版本和超长输入会在返回错误后关闭连接。

任务请求包括 `submit`、`snapshot`、`logs`、`confirm` 和 `cancel`。`submit` 使用枚举操作，参数中没有任意 shell 或提权命令入口。`confirm` 要求任务处于 `waiting_confirmation` 且步骤匹配。对终态任务发送 `cancel` 会返回现有快照。
