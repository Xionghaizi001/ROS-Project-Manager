# Phase 3.6 常见错误诊断验收

日期：2026-10-24。结果：**通过**。范围：Phase 3.6 最后一项真实工具 / CLI 验收，包含已完成的日志采集、解析规则、摘要及未知格式兜底的回归。

## 环境与方法

Ubuntu 22.04.5 x86_64、ROS Humble、colcon-core 0.21.3 / colcon-cmake 0.2.30，真实工具版本如下：

| 工具 | 版本 |
| --- | --- |
| GCC / G++ | 11.4.0 |
| Clang / Clang++ | 14.0.0（Ubuntu 1ubuntu1.1） |
| CMake | 3.22.1 |
| GNU ld | 2.38 |
| Python | 3.10.12（`/usr/bin/python3 -I -S`） |

[p36-diagnostics-check.py](../scripts/p36-diagnostics-check.py) 在全新临时目录生成两套含空格路径的最小项目，分别使用 GCC 和 Clang。每套有 11 个 colcon CMake 包：6 个 C++ / CMake / 链接失败包、3 个 Python 失败包、1 个未知格式失败包和 1 个成功包。Python 文件由 CMake 自定义目标真实执行，确保 traceback 来自构建阶段并进入 colcon 按包日志；本项不验证 Python 包安装器。

脚本通过真实 `rospm build -- --continue-on-error --parallel-workers 4 --event-handlers console_direct+` 执行多包构建，不伪造编译器、Python 或 colcon 输出。清除调用端 Conda / ROS 激活变量，使用系统 PATH；只在新目录写源码、产物和证据，不执行 `apply` 或安装系统依赖。本机缺少 Clang，因此将 Ubuntu 的 clang-14、libllvm14、libclang-cpp14、libclang-common-14-dev、libclang1-14 下载解包至忽略目录 `.tools/p36-clang/root`，通过 `LD_LIBRARY_PATH` 加载其库，未修改系统安装。

## 断言与结果

每个编译器运行 9 次 CLI，共 **18 次**：多包失败的中英文 × JSON / 文本 4 次；未知格式的中英文 × JSON / 文本 4 次；保留历史失败日志后成功构建 1 次。

| 场景 | 核对内容 |
| --- | --- |
| GCC / Clang 缺分号 | `syntax_error` / `syntax`，保留工具报告的 main.cpp 位置；重复原文出现至少两次，摘要仅一条 |
| 缺头文件 | `missing_header` / `header_search`，第 1 行，建议检查 include 路径与声明 |
| 模板返回类型错误 | `type_error` / `type`，实例化 / note 上下文保留于 `related` |
| CMake `find_package` | `package_not_found` / `package_discovery`，建议核对 package.xml、`rospm deps check` 与 underlay |
| ld 缺库 / 未定义引用 | `library_not_found` / `undefined_reference`，类别为 `linking`，建议检查链接配置；缺库无文件位置时为 `null` |
| Python SyntaxError / ModuleNotFoundError | 真实文件第 1 行、对应类别与建议；缺模块提示检查 `sys.executable`、`sys.path`，不直接生成 pip 安装命令 |
| Python KeyError → ValueError | 最终异常第 7 行，保留异常链与调用帧；子进程未结束前已收到其主动 flush 的标记，证明实时转发 |
| 多包并行、重复与级联 | 按实际包日志归组，workspace 为 main；合并流不重复已有诊断，make / colcon 后续结果单独保留 |
| 空格路径、ANSI、原始日志 | 每条保留的原始片段逐行出现在包日志及合并日志，ANSI 字节保留；每次执行的完整日志另存证据目录 |
| 未知格式 | ANSI 原文仍在 stderr 与日志；文本提示查看日志，JSON 为 `unrecognized`、空诊断，不混入之前包的诊断 |
| 成功与旧日志 | 成功执行返回唯一 JSON `{"ok": true}`，无旧失败摘要 |
| 中英文与失败状态 | 文本显示相应语言标题及完整日志路径；stdout JSON 可独立解析，原始输出走 stderr；CLI 失败退出 1，JSON 保留实际 colcon 状态 |

本机真实 colcon 在上述多包失败时返回 1，单个未知包的 make 失败时返回 2；均核对 JSON `status`、`error` 与原始日志中的状态一致，没有将工具退出码与 rospm 的失败退出码混为一谈。

补充边界由现有 `diagnostic::cpp` / `diagnostic::python` 单元测试及 CLI `build` 集成测试覆盖：include 链、中文 GCC 缺头文件、Clang note 归属、零 / 缺失行号、Python 缩进 / TabError / ImportError / 自定义异常、显式与隐式异常链、成功文本不误报、旧包日志与先前命令隔离、缺失日志 / 命令边界、完整 JSON 与原失败状态。`real_python_failures` 使用真实 Python 运行七类失败样例。

Clang 默认源码摘录中不带 `|` 的行仍不并入摘要 `raw`（既有已记录限制）；验收逐行核对被保留的片段，完整源码摘录保留在原始日志。摘要不是完整日志或唯一根因判定；超过 4 MiB 的解析范围、交错 traceback 和 ExceptionGroup 等仍遵循 `spec.md` 的覆盖约定。本项验证构建诊断，不运行会修改系统的一键还原；`apply --build` 复用同一构建执行 / 摘要路径，其预览与 JSON 约定另由 CLI 回归覆盖。

## 复跑

先准备 ROS Humble、colcon CMake 扩展、CMake、GCC 和 Clang，然后在仓库根目录执行：

```bash
cargo build -p rospm-cli
python3 scripts/p36-diagnostics-check.py \
  --root /tmp/p36-new-run \
  --clang /usr/bin/clang++-14
```

`--root` 必须不存在。脚本缺少任一前置工具会失败，不会跳过后报告通过。也可以运行专用 Cargo 集成入口：

```bash
P36_CLANGXX=/usr/bin/clang++-14 \
  cargo test -p rospm-cli --test diagnostics_acceptance -- --ignored --nocapture
```

`P36_GXX` 可覆盖 GCC 路径；默认普通 `cargo test --workspace` 将此项标记 ignored，避免要求所有 CI 主机安装 ROS 和两套编译器。该入口调用同一脚本，测试结束保留项目与证据目录。

本机解包的 Clang 可用以下参数复跑：

```bash
P36_CLANGXX="$PWD/.tools/p36-clang/root/usr/bin/clang++-14" \
LD_LIBRARY_PATH="$PWD/.tools/p36-clang/root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  cargo test -p rospm-cli --test diagnostics_acceptance -- --ignored --nocapture
```

证据目录包含 `versions.json`、`results.json`、每次 CLI 的 `.command`、`.stdout`、`.stderr` 和 `.build.log`；colcon 按包日志保留于各工作区 `ws/log/`。

本次通过的真实集成测试证据：`/tmp/rospm-p36-4146962-1792841726839168078/evidence`，18 次 CLI 执行通过，专用 Cargo 集成测试 **1/1 通过**。

最终回归：`cargo test --workspace` 全部通过（普通运行中真实验收入口为 ignored，已按上文命令单独通过）；`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check` 与 `git diff --check` 通过。工作区测试日志：`/tmp/rospm-p36-workspace-tests.log`。
