# v1.1.0

本次更新新增终端交互界面（TUI），将安装、构建等操作接入独立 worker，并补齐项目运行、进程管理与 Python 虚拟环境支持。[查看完整提交差异](https://github.com/Xionghaizi001/ROS-Project-Manager/compare/v1.0.0...47d17f1bb03a94b9f288fef589feb7f1a128e38b)。

## 新功能

- **终端交互界面**：新增 `rospm tui`，支持键盘导航、鼠标点击与滚动，可扫描并打开项目、创建项目、检查环境与依赖、还原、验证、编译和重新编译。内置中英文 TUI / CLI 使用说明，无项目时也可查看帮助。
- **可视化安装入口**：在 TUI 中安装 ROS、Conda / Miniforge、项目组件及 rosdep 依赖，创建 `.venv` 并从 requirements 文件安装 Python 依赖；支持选择构建和运行所用的 Python 环境。
- **独立 worker 与任务管理**：新增 `rospm worker` 和 `rospm task`，支持提交、查询状态、读取日志、确认、取消及 `attach` 接入任务。同步 CLI 的构建、还原、卸载、依赖安装、venv 操作和锁文件生成也通过 worker 执行。
- **任务持久化与重连**：保存任务状态、步骤进度、结果和日志；worker 运行期间，退出界面或断开客户端后仍可用原任务 ID 重连。支持重复提交去重、历史结果离线查看和空闲自动退出；worker 重启后标记未完成任务状态，不自动重放操作。
- **工作区并发协调**：CLI、TUI 与 worker 共用工作区锁，安装、卸载、构建等修改操作互斥，只读操作可共享访问；运行节点不会长期占用环境修改锁。
- **前台系统授权**：安装等系统步骤可等待前台终端授权，由 sudo 直接读取密码；TUI 暂停后接入原任务，完成后返回界面。Git、Conda、构建和项目 shell 继续以当前用户身份运行。
- **运行与进程管理**：新增 `rospm run`，可列出可执行目标、交互选择并用 Tab 补全，支持传入运行参数。TUI 可前台或后台运行节点、执行自定义命令、进入项目 shell，并查看运行历史、日志及停止、强制结束、暂停或恢复运行进程。按 `p` 打开进程管理，按 `l` 查看日志。
- **项目 Python 虚拟环境**：新增 `rospm venv create` / `install`，支持标准 Python 工具和已有 uv；`env`、`shell`、`build`、`run` 可通过 `--venv` / `--venv-path` 选择环境。`check`、`verify`、`doctor` 与构建诊断增加 venv、Python 兼容性和旧 CMake Python 缓存检查。

## 问题修复

- 修复重新打开 TUI 后无法继续原任务的问题，可查看等待确认或授权的操作、接入原任务日志并请求取消；worker 不可用时显示已保存状态，不重复提交任务。
- 修复 TUI 编译 / 重新编译未通过独立 worker 执行的问题；安装页操作与依赖补全同样接入任务执行流程。
- 修复 rosdep 缓存索引缺失时检查无法自动准备缓存的问题：本机检查会按项目 ROS 发行版更新当前用户缓存，`ROSPM_NO_NETWORK=1` 可跳过更新；同时修复 TUI 依赖补全未正确调用 rosdep 安装的问题。
- 修复扁平 workspace 被假定必须包含固定源码子目录，导致项目识别、依赖检查和构建失败的问题。
- 修复 worker 取消请求未及时反映到任务状态、取消信号未覆盖构建子进程组的问题；受保护的系统安装步骤仍在安全边界响应取消。
- 修复同步 CLI 绕过工作区互斥、与 worker 同时修改项目的问题，并调整运行操作的锁范围。
- 修复并发日志写入损坏、日志游标未更新、UTF-8 字符跨数据块乱码，以及遗留临时快照文件阻碍恢复的问题；为内存日志缓冲增加上限。
- 修复 TUI 表格长内容破坏布局、表单鼠标操作无响应、焦点与选中项显示不一致的问题；修复鼠标退出后控制字符残留在 shell 输入中的问题。
- 修复 `rospm run` 交互补全时换行与输入行重绘异常的问题。

## 其他调整

- 精简命令执行输出，默认显示当前步骤，`-v` / `-vv` 显示完整命令；保留子进程输出、错误信息和构建结果提示。
- 调整 TUI 日志查看器布局，统一底部快捷键样式，提供可点击的日志与进程管理入口，并整理编译 / 重新编译操作提示。
- 发布附件改为直接提供二进制文件：x86_64 下载 `rospm_1.1.0.x86`，aarch64 下载 `rospm_1.1.0.arm64`。

## 升级与使用

替换原有 `rospm` 可执行文件即可升级，`rospm.toml` / `rospm.lock` 继续使用 schema v1，无需修改格式。使用说明见 [README](https://github.com/Xionghaizi001/ROS-Project-Manager/blob/main/README.md) 和 [worker CLI 使用说明](https://github.com/Xionghaizi001/ROS-Project-Manager/blob/main/docs/worker-cli.md)。

```sh
rospm --version             # rospm 1.1.0
rospm tui                   # 打开终端界面
rospm run --list            # 列出可执行目标
rospm build --venv          # 使用项目已有 .venv 构建
```

关闭 TUI 不等于取消环境任务，应通过任务管理显式请求取消；已完成的修改不会自动回滚。venv 依赖尚不纳入 `apply --locked` 重放，`pyproject.toml` / `uv.lock` 不会自动同步。arm64 本次仍不声明完成原生实机验收。

---

## English

This release adds a terminal user interface (TUI), moves installation and build operations to an independent worker, and introduces project execution, process management, and Python virtual environment support. [View the full comparison](https://github.com/Xionghaizi001/ROS-Project-Manager/compare/v1.0.0...47d17f1bb03a94b9f288fef589feb7f1a128e38b).

### New features

- **Terminal user interface**: Added `rospm tui` with keyboard navigation, mouse clicks, and scrolling. Scan for and open projects, create projects, check environments and dependencies, restore, verify, build, and rebuild. Includes Chinese and English TUI / CLI guides, with help available even when no project is open.
- **Installation through the TUI**: Install ROS, Conda / Miniforge, project components, and rosdep dependencies; create `.venv` environments and install Python dependencies from requirements files. Select the Python environment used for builds and execution.
- **Independent worker and task management**: Added `rospm worker` and `rospm task` for submitting tasks, querying status, reading logs, confirming operations, cancelling tasks, and connecting with `attach`. Synchronous CLI commands also use the worker for builds, restores, uninstalls, dependency installation, venv operations, and lock file generation.
- **Persistent tasks and reconnection**: Task state, step progress, results, and logs are saved. While the worker is running, clients can reconnect using the original task ID after leaving the interface or disconnecting. Supports submission deduplication, offline access to historical results, and automatic exit when idle. After a restart, the worker marks the state of unfinished tasks without automatically replaying operations.
- **Workspace concurrency coordination**: The CLI, TUI, and worker share workspace locks. Operations that modify the workspace, such as installation, uninstallation, and builds, are mutually exclusive, while read-only operations can share access. Running nodes do not hold the environment mutation lock for their entire lifetime.
- **System authorization in the foreground**: System steps such as installation can wait for authorization in a foreground terminal, where sudo reads the password directly. The TUI suspends to attach to the original task and returns afterward. Git, Conda, builds, and project shells continue to run as the current user.
- **Execution and process management**: Added `rospm run` to list executable targets, select them interactively with Tab completion, and pass runtime arguments. The TUI can run nodes in the foreground or background, execute custom commands, open a project shell, show execution history and logs, and stop, force-terminate, suspend, or resume running processes. Press `p` to open process management and `l` to view logs.
- **Project Python virtual environments**: Added `rospm venv create` / `install`, supporting standard Python tools and an existing uv installation. Use `--venv` / `--venv-path` to select an environment for `env`, `shell`, `build`, and `run`. Added venv, Python compatibility, and stale CMake Python cache checks to `check`, `verify`, `doctor`, and build diagnostics.

### Bug fixes

- Fixed the inability to continue an existing task after reopening the TUI. Users can inspect operations awaiting confirmation or authorization, attach to the original task's logs, and request cancellation. When the worker is unavailable, the TUI shows saved state without resubmitting tasks.
- Fixed TUI build / rebuild actions bypassing the independent worker. Installation page actions and missing-dependency installation also use the task execution flow.
- Fixed checks failing to prepare the rosdep cache automatically when its index is missing. Local checks update the current user's cache for the project's ROS distribution; set `ROSPM_NO_NETWORK=1` to skip the update. Also fixed TUI missing-dependency installation failing to invoke rosdep correctly.
- Fixed project detection, dependency checks, and builds failing for flat workspaces because a fixed source subdirectory was assumed.
- Fixed worker cancellation requests not being reflected promptly in task state and cancellation signals not reaching the build process group. Protected system installation steps still respond to cancellation at safe boundaries.
- Fixed synchronous CLI commands bypassing workspace exclusion and modifying a project concurrently with the worker. Also adjusted the lock scope for execution operations.
- Fixed corruption from concurrent log writes, log cursors failing to advance, garbled UTF-8 characters split across data chunks, and leftover temporary snapshot files blocking recovery. Added a limit to in-memory log buffers.
- Fixed long table content breaking the TUI layout, forms not responding to mouse input, and inconsistent focus and selection display. Also fixed mouse control sequences being left in shell input after exiting the TUI with the mouse.
- Fixed newline handling and input-line redraw issues during interactive completion in `rospm run`.

### Other changes

- Simplified command output: the current step is shown by default, while `-v` / `-vv` shows full commands. Child process output, errors, and build result messages are preserved.
- Updated the TUI log viewer layout, unified footer shortcut styling, added clickable log and process management entries, and refined build / rebuild action hints.
- Release assets are now provided directly as binaries: download `rospm_1.1.0.x86` for x86_64 or `rospm_1.1.0.arm64` for aarch64.

### Upgrading and usage

Replace the existing `rospm` executable to upgrade. `rospm.toml` / `rospm.lock` continue to use schema v1, with no format changes required. See the [README](https://github.com/Xionghaizi001/ROS-Project-Manager/blob/main/README.md) and [worker CLI guide](https://github.com/Xionghaizi001/ROS-Project-Manager/blob/main/docs/worker-cli.md) for usage instructions.

```sh
rospm --version             # rospm 1.1.0
rospm tui                   # Open the terminal interface
rospm run --list            # List executable targets
rospm build --venv          # Build using the project's existing .venv
```

Closing the TUI does not cancel environment tasks; request cancellation explicitly through task management. Completed changes are not rolled back automatically. venv dependencies are not yet included in `apply --locked` replay, and `pyproject.toml` / `uv.lock` are not synchronized automatically. This release still does not claim completed acceptance testing on native arm64 hardware.
