# P4.5.2 worker 原型验收

日期：2026-10-31。

`scripts/p45-worker-prototype.sh` 是第一项待办的进程生命周期原型，不是最终 worker、Unix socket 协议或任务存储实现。运行：

```sh
scripts/p45-worker-prototype.sh
```

原型以当前用户启动一个 controller（模拟 TUI），controller 用 `setsid` 启动 worker，worker 再用独立 `setsid` 启动测试子进程。controller 在子进程完成前被强制结束，随后向 worker 发送 `SIGHUP`；测试要求 worker 和子进程都完成，并检查：

- worker 和子进程的 UID 等于当前用户，没有隐式提权；
- worker、子进程分别拥有独立的进程组；
- controller、worker、子进程的标准输入均为 `/dev/null`，没有继承 TUI 或 SSH 的终端输入；
- controller 退出和 worker 收到 `SIGHUP` 都不会中断任务；
- 子进程完成后 worker 才退出，生命周期由 worker 持有。

关闭 controller 的三个文件描述符并使用 `/dev/null`，只模拟 SSH 登录描述符关闭及 TUI 断开后的必要条件；它不等价于真实 sshd、网络中断、tmux 或主机重启。原型也没有证明 worker 崩溃后的任务恢复、跨 worker 重连、日志持久化、进程组内后代回收或安装步骤的安全取消。生产实现仍须在 4.5.2 后续待办和 4.5.4 故障验收中补齐这些边界。
