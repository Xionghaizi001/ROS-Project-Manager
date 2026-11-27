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

CLI 使用任务 ID 作为请求 ID。ID 长度为 1–128 个字符，可使用 ASCII 字母、数字、点、下划线和连字符，单独的 `.` 和 `..` 除外。提交成功返回 `accepted`，表示请求已接收。用 `status` 查看快照中的 `state`、当前 `step`、总步骤数 `total_steps` 和最终 `result`。`total_steps` 未知时为 `null`。提交、查询和控制请求返回协议 v1 JSON，无需添加 `--json`。`attach` 在终端显示日志和交互提示。查询到失败任务不会让查询命令本身失败。

常用操作包括 `apply`、`apply_locked`、`build`、`rebuild`、`uninstall`、`check`、`verify` 和 `deps`。可通过 `rospm task --runtime /tmp/rospm-my-project submit --help` 查看可选值。

- 使用清单默认配置和官方镜像。`build` 构建整个 workspace，不隐含执行 `build.verify`。
- `rebuild` 在 workspace 租约内校验并删除清单指定的 colcon build / install 目录，再构建整个 workspace；保留源码。提交该操作即表示同意清理这些产物。
- `apply_locked` 和 `verify` 需要与清单匹配的锁文件。规划错误也会写入失败结果。
- `check`、`verify` 和 `deps` 将检查报告写入任务日志。环境不满足要求时，任务状态为失败。`deps` 只检查依赖，不执行安装。
- `build` / `rebuild` 使用 `--interactive` 提交时，构建失败后可进入 `waiting_confirmation`，确认清理产物后重试；未批准不会删除或重试。非交互提交直接记录构建失败。
- worker 不提供同步 CLI 的临时参数覆盖、镜像自动选择和 `apply --build` 组合。需要这些选项时使用对应的同步命令。
- 默认提交使用非交互 `sudo -n`。权限不足会记录为失败。

同一项目中的任务按顺序取得 workspace 租约，租约覆盖规划、确认等待和结果写入。最多保留 32 个活动或排队任务。排队任务可以直接取消。

TUI 编译页的 `b` / `B` 及对应鼠标按钮分别提交 `build` / `rebuild`。构建、环境准备、依赖安装及其他 worker 操作提交成功后，立即在现有进程管理页面打开新任务，显示任务名称 `task/<任务 ID>`、状态和操作入口。提交过程保持在 TUI 中，不自动显示终端输出，也无需按 Enter 返回；按 `l` 可查看该任务的日志。暂停或关闭 TUI 不取消已提交的构建；可使用显示的任务 ID 通过 `task status`、`logs`、`attach` 或 `cancel` 继续操作。

## 在 TUI 恢复任务

重开同一项目的 `rospm tui`，按 `p` 打开进程管理。界面会读取已保存的任务快照，显示活动任务和历史结果，区分等待确认、等待授权、已请求取消、成功、失败、已取消、中断和状态未知。打开界面不会重新提交任务，也不会替任务确认或授权。

选中活动任务并按 Enter，或用鼠标打开任务详情：

- **继续任务／查看待确认操作**：跟随原任务日志，显示完整待确认操作后等待输入；需要系统权限时切换到前台终端授权。结束后按 Enter 返回 TUI。
- **取消任务**：发送 worker 的任务取消请求，在对应安全边界生效。环境任务不提供进程的强杀、暂停或恢复按钮。
- 按 `l` 直接打开选中任务的持久化日志；历史任务也可以查看日志和最终结果。

TUI 使用项目默认端点 `.rospm/worker/runtime`。连接不可用时显示“无法连接 worker；上次保存状态”，不提供活动任务控制，也不会自动启动 worker 或修改旧快照。成功、失败、已取消等已保存终态仍可离线查看。显式启动在自定义 `--runtime` 的 worker，请用带同一 `--runtime` 的 CLI 命令控制。

## 确认和取消

卸载任务会停在 `waiting_confirmation`。先读取日志中的完整操作 JSON，再使用待确认步骤的实际编号回复

```sh
rospm task --runtime /tmp/rospm-my-project submit uninstall --id remove-001
rospm task --runtime /tmp/rospm-my-project logs remove-001
# 将 2 换成快照中待确认的 step
rospm task --runtime /tmp/rospm-my-project confirm remove-001 --step 2 reject
```

确认响应只能是 `approve`、`reject` 或 `cancel`。错误步骤、重复响应和已经取消的确认会被拒绝。客户端断开不会批准或取消任务。任务运行期间可以使用 `cancel` 请求取消，实际生效时间遵循执行步骤的取消边界。apt / dpkg 等受保护步骤完成当前命令后才响应取消；已完成的系统更改不会回滚。

## 系统权限

以普通用户运行 `rospm tui`。启动时通过 `sudo -l` 检查当前用户，检查失败时按 Esc 可继续使用 TUI。桌面会话中，sudo 在需要密码时调用系统的图形 askpass 程序，并按 sudoers 策略完成认证。可通过 `SUDO_ASKPASS` 或 `sudo.conf` 配置该程序，Ubuntu 可使用 `ssh-askpass-gnome`。无图形会话时，TUI 可执行 sudo 已授权的免密码步骤。当前 TUI 提交的任务在后台处理授权和系统步骤，期间仍可浏览任务、查看日志或请求取消。

系统步骤在 `waiting_authorization` 等待前台客户端接入。每次 sudo 调用都以当前 sudoers 策略为准。Git、Conda、构建与项目 shell 以当前用户运行。rosdep 安装也以当前用户启动，由 rosdep 为系统包操作调用 sudo。

交互式同步 CLI 和 `rospm task attach` 使用控制终端认证。显式 worker 提交可以用 `--interactive` 让系统步骤等待终端，再用 `attach` 接入同一任务。

```sh
rospm task --runtime /tmp/rospm-my-project submit components_install --interactive --id install-001
rospm task --runtime /tmp/rospm-my-project attach install-001
```

没有终端接入时，任务保留等待状态，可取消或用原 ID 接入。系统步骤已交给前台、随后连接丢失且 worker 收不到结果时，任务标记为 `unknown`。重试前需要核对系统状态和任务日志。

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

worker 停止后，在项目目录中用原来的 `--runtime` 运行 `status` 可离线查询终态，`logs` 可读取已保存的日志。上次故障留下的 `pending`、`running`、`waiting_confirmation`、`waiting_authorization` 和 `cancel_requested` 任务会在 CLI worker 重启时标为 `unknown`，已结束任务保持原状态。库接口 `TaskStore::recover_active_tasks` 还支持在调用方能确定任务已随 worker 退出而中断时使用 `RecoveryCause::WorkerExit`，将活动任务标为 `interrupted`。

恢复只更新快照。旧子进程可能仍在运行，等待中的确认需要随新任务重新处理。先核对日志、系统状态和旧子进程，再决定是否用新 ID 显式提交任务。

## 任务存储与提交去重

任务数据保存在项目的 `.rospm/worker/tasks/`，每个任务在 `<task-id>/` 下保存 `snapshot.json` 和 `logs.jsonl`。任务目录权限为 `0700`，两个文件权限为 `0600`。快照通过写入临时文件、同步并原子替换保存。worker 退出后保留任务数据，`--runtime` 只用于本机通信和运行时锁。同步命令的 `.rospm/log/build.log` 仍按原方式覆盖。

worker 在分派前持久化提交账本 `@submissions.json` 和初始 `pending` 快照，并使用 `@submissions.lock` 串行注册。CLI 提交响应丢失时，可用原任务 ID、项目和操作重试。直接使用协议时，重试还须原样保留 `request_id`、`task_id` 和 `workspace`。完全一致的请求只返回原 `accepted`，更改请求内容或占用已有任务 ID 会返回 `submission_conflict`。

每个请求最多分派一次。若账本已写入但未成功创建快照，重试会返回 `task_unavailable`。分派被拒绝会记为 `failed`，同一请求的重试不会再次入队。账本损坏会导致提交失败，worker 会保留原账本。

CLI worker 会保留已结束任务，不自动轮转或清理磁盘日志。库接口 `TaskStore::prune_completed` 可按数量清理最旧的终态任务目录，同时保留活动任务和去重账本。已清理任务的旧 ID 仍占用，重复提交返回 `task_unavailable`。

## 本机 socket 协议

`--runtime` 须由当前用户拥有，目录本身不得为符号链接。worker 将目录权限设为 `0700`，`worker.sock` 权限设为 `0600`。路径应足够短，以满足 Unix socket 的长度限制。一个项目和一个 runtime 各自只允许一个 worker 占用。CLI worker 取得两把锁后才会清理无法连接的残留 socket，遇到同名非 socket 路径会报错。Linux 上通过 `SO_PEERCRED` 校验对端的有效 UID 与 worker 一致。

协议在 Unix stream 上传输以换行分隔的 JSON，每帧带 `version = 1`，含换行最多 1 MiB。客户端先发送 `hello`，收到 `client` 为 `rospm-worker` 的 `hello` 响应后，再逐条发送请求并读取响应。服务最多保留 32 个连接，超出上限的新连接会被关闭。无效 JSON、未知版本和超长输入会在返回错误后关闭连接。

任务请求包括 `submit`、`snapshot`、`logs`、`confirm` 和 `cancel`。`submit` 使用枚举操作，可选的 `interactive` 默认关闭，重试须保留原值。`confirm` 要求任务处于 `waiting_confirmation` 且步骤匹配。对终态任务发送 `cancel` 会返回现有快照。
