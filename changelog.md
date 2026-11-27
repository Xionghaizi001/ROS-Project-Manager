# v1.1.1

TUI 的「调试」页可以检查、启动和安装常用 ROS 图形工具。受管 Conda 环境改用用户数据目录，rospm 安装的 Miniforge 也可以设为当前用户的默认 Conda。[查看完整提交差异](https://github.com/Xionghaizi001/ROS-Project-Manager/compare/v1.1.0...cdbe7c50435b993a54e6bcff81422296a58d0914)

## 新功能与改进

- 在 native 项目的「调试」页查看工具状态并启动 `rqt`、`rviz2`、`rqt_graph`、`rqt_topic`、`rqt_publisher`、`rqt_service_caller`、`rqt_reconfigure` 和 `rqt_console`。工具加载项目的 ROS 发行版和 workspace overlay，并使用系统 Python 和 Qt。
- 调试工具缺失时，可在 TUI 中确认安装对应 ROS 发行版的 apt 包，也可运行 `rospm deps install --tool <name>`。加上 `--dry-run` 可预览安装计划。
- TUI 在界面内显示操作确认面板。按 Enter 确认、按 Esc 取消，也可点击底部相应按钮。
- 在 `rospm.toml` 的 `[dependencies]` 中填写 `apt` 列表，常规 `rospm apply` 会为 native 项目安装其中声明的调试工具、可选组件等额外包。两种 `init` 模式均生成空列表，由项目开发者按需填写。
- 新建的受管 Conda 环境默认存放在 `~/.local/share/rospm/conda/envs/`，支持 `XDG_DATA_HOME`，并按项目清单所在目录和 Conda 平台隔离。已有 `.rospm/conda/ready.sha256` 记录的项目继续使用原位置。
- 可用 `rospm conda set-default` 将 rospm 安装的 Miniforge 设为当前用户的默认 Conda，再用 `rospm conda remove-path` 撤销。TUI 的「安装 → Conda」也提供这两项操作，设置对新启动的 Bash、Zsh 和 Fish 会话生效。

## 问题修复

- 调整 TUI 切页和从外部命令返回时的屏幕恢复，减少 GNOME Terminal 中的闪烁。
- 将 TUI 交互 Shell 中收起会话的命令更正为 `rospm background`。重新打开 Shell 可接回原会话。

## 升级

按架构选择 `rospm_1.1.1.x86`（x86_64）或 `rospm_1.1.1.arm64`（aarch64），替换原有可执行文件。现有 schema v1 的 `rospm.toml` 和 `rospm.lock` 可直接使用。[使用文档](https://github.com/Xionghaizi001/ROS-Project-Manager/blob/main/README.md)

---

## English

The TUI Debug page can check, launch, and install common ROS graphical tools. Managed Conda environments now use the user data directory, and you can set rospm-installed Miniforge as the current user's default Conda. [View the full comparison](https://github.com/Xionghaizi001/ROS-Project-Manager/compare/v1.1.0...cdbe7c50435b993a54e6bcff81422296a58d0914)

### New features and improvements

- For native projects, check tool status and launch `rqt`, `rviz2`, `rqt_graph`, `rqt_topic`, `rqt_publisher`, `rqt_service_caller`, `rqt_reconfigure`, and `rqt_console` from the Debug page. Tools load the project's ROS distribution and workspace overlays and use system Python and Qt.
- When a debug tool is missing, confirm installation of the apt package for your ROS distribution in the TUI, or run `rospm deps install --tool <name>`. Add `--dry-run` to preview the installation plan.
- The TUI displays an in-app confirmation panel. Press Enter to confirm or Esc to cancel, or click the corresponding footer button.
- Declare extra apt packages, such as debugging tools and optional components, in `[dependencies].apt` in `rospm.toml`. A regular `rospm apply` installs the listed packages for native projects. Both `init` modes generate an empty list for project developers to fill in.
- New managed Conda environments default to `~/.local/share/rospm/conda/envs/`, respect `XDG_DATA_HOME`, and are isolated by manifest directory and Conda platform. Projects with an existing `.rospm/conda/ready.sha256` marker continue to use their original location.
- Run `rospm conda set-default` to set rospm-installed Miniforge as the current user's default Conda, or `rospm conda remove-path` to undo it. Both actions are available under Install → Conda in the TUI and apply to new Bash, Zsh, and Fish sessions.

### Bug fixes

- Updated screen restoration when switching TUI pages or returning from external commands to reduce flicker in GNOME Terminal.
- Corrected the command for hiding an interactive shell session in the TUI to `rospm background`. Reopen Shell to resume the session.

### Upgrading

Replace your existing executable with `rospm_1.1.1.x86` for x86_64 or `rospm_1.1.1.arm64` for aarch64. Existing schema v1 `rospm.toml` and `rospm.lock` files work as-is. [Usage documentation](https://github.com/Xionghaizi001/ROS-Project-Manager/blob/main/README.md)
