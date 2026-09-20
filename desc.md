定位：用于“描述”ROS环境的工具
目标：
- 用户与场景：单人开发 / 实验室团队共享 / CI / 部署到机器人本体
- 支持的平台（暂定）：Ubuntu / ROS2 only，x86_64 与 aarch64
- 与现有工具的关系：封装而非代替
- 对ROS项目进行可复现的描述，包括
	- 使用的ROS版本
	- 环境提供者（Pixi、Docker、native等）
	- 依赖管理器（rosdep / system deps）
	- 使用的软件包
	- 源代码管理器（git / vcs / svn …）
	- 编译器（colcon / catkin） / 基于环境提供者的编译器
	- 环境变量管理（ROS_DOMAIN_ID, RMW_IMPLEMENTATION, AMENT_PREFIX_PATH, COLCON_PREFIX_PATH, …）
	- 使用的编辑器（vscode / visual studio / clion ...）
- 提供对描述文件的重放
- 对软件包版本进行固定
	- lock文件功能：固定软件包版本号，从软件源获取
	- 依赖补全：根据项目描述文件或包记录从安装源中补全缺失的依赖
- 对跨平台软件包提供自动替换支持
- 其他与之相关的周边功能
- 提供CLI（必备）和用户友好的GUI（选装）
- 离线场景支持：在联网环境下生成带有依赖包的导出文件（类似于docker）
实现语言：rust / c++

这一项目的最终目的是只通过项目管理器本身以及项目的描述文件（含源码）即可在不同设备甚至不同架构的平台（x86 / arm）上，快速地部署或恢复一个ROS项目，并提供依赖检查和补全等周边功能。