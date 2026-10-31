# P4.5.2 worker 任务存储与日志

日期：2026-10-31。

`rospm-core::worker::TaskStore` 在用户私有目录下为每个任务建立独立目录：

```text
<worker-runtime>/tasks/<task-id>/
  snapshot.json
  logs.jsonl
```

目录权限为 `0700`，快照和日志仅由当前用户读写。快照通过临时文件写入、`sync_all` 后原子 rename，断电或 worker 退出不会把旧快照截断。日志采用追加 JSONL，每条记录带单调 `cursor`、stdout/stderr/worker 流类型和文本；重连从游标读取增量，读取上限由调用方传入，避免一次加载无界历史。日志追加完成后执行 `sync_data`，写入失败直接返回错误。

任务结束后快照和日志仍保留，worker 退出不删除任务目录。worker 启动或任务完成时可调用 `prune_completed(max_completed)`，只按最旧目录清理已结束任务，活动任务永不清理；建议默认保留最近 100 个已结束任务。生产部署可在此基础上增加按天数和磁盘配额策略，但必须先记录清理结果。

现有 `.rospm/log/build.log` 继续由 CLI 按原语义覆盖，用于兼容现有调用者。它不是任务历史，也不作为 `TaskStore` 的日志文件；worker 产生的 stdout/stderr 必须写入对应任务的 `logs.jsonl`，需要兼容展示时再显式复制或汇总，不能把覆盖文件当作重连游标来源。
