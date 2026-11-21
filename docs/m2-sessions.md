# 多会话（2026-11-11）

M1 遗留的"只有 `s-chat` 一个会话"到这里补上：侧栏列出目录里所有会话，可新建、切换、
重命名、删除，重启后恢复。

## 存储布局

```text
.tmp/sessions/                 # DSHR_SESSIONS_DIR 可覆盖
  s-1794329101557.jsonl        # Session Log（append-only，文件名 stem = session_id）
  s-1794329101557.meta.json    # 可选：{ "title": "..." }，重命名只写这里
```

- 事件文件仍然是 `SessionLog` 的 JSONL，格式/不变量不变（`docs/spec/session-log.md`）。
- **重命名不碰 append-only 的文件**，避免为了改名重写历史；标题优先取 meta，其次取第一条
  `message.user` 的文本。
- 侧栏的"更新时间"取文件 mtime，UI 显示相对时间（刚刚 / N 分钟前 / N 小时前 / N 天前），
  不引入日期库。

实现在 [`crates/session-store/src/session_index.rs`](../crates/session-store/src/session_index.rs)
（`SessionIndex` / `SessionMeta`），6 个目录索引测试覆盖：标题推导、缺目录、重命名覆盖、
创建/删除、ID 不冲突、按时间倒序。

## 启动与迁移

1. 默认目录 `.tmp/sessions`；不存在则创建。
2. 如果目录为空且存在旧的单会话文件 `.tmp/chat-session.jsonl`，复制为 `s-chat.jsonl`
   （升级不丢历史）。
3. 仍为空则新建一个会话。
4. 打开更新时间最新的会话。

## 切换语义

- 桌面窗口复用一套工具运行时（含 Node sidecar 和插件注册表）。
  切换时加载目标会话的日志并更新对话视图。已有会话的普通切换复用侧栏索引。
- **生成中禁止切换**：工作线程还在往旧 state 写，切走会让 UI 与日志分叉。
- Sidecar 工具调用携带实际会话 ID，宿主服务按该 ID 查询会话及工作目录。
  调用超时按请求取消，窗口继续复用进程。插件配置变化会触发重载，进程退出后在下一轮发送前恢复。

## UI

- 侧栏：标题 + "N 条 · 相对时间"，当前会话高亮；`✎` 进入行内重命名（✓ 提交 / ✕ 取消），
  `×` 进入二次确认（删除 / 取消），避免误删。
- `＋ 新建对话`：生成 `s-<毫秒>` 并切过去。
- 头部面包屑显示当前会话标题（长标题截断到 220 逻辑像素，避免和右侧状态挤在一起）。
- 新会话给的是空态页（DSH 的"开始对话"引导）。

## 会话树

- 新会话首条事件写 `session.created`（workspace/父链/来源），子会话由 `spawn_child`
  创建为独立 JSONL 并带 `parent_session_id`。
- 侧栏按 `SessionTree::flatten()` 的前序排序，子会话缩进 `depth × 14px` 并加 `↳`；
  模型可在工具链里派发子会话，父日志出现 `child.spawned` / `child.finished`。
- 中断传播：用户停止时子轮次共享同一个 cancel 句柄，子会话落
  `session.interrupted_by_parent`，父会话落 `session.interrupted`。
- 详见 [`m2-session-tree.md`](m2-session-tree.md)；`--selftest-tree` 覆盖端到端链路。

## 导出与保留

```powershell
# 导出：JSONL + 外部化附件 + manifest（sha256），导出后自动 verify
dshr --export-session <会话 id> --export-dir .tmp\exports

# 保留计划：只计算不删除（默认 2GiB / 90 天 / 保护最新 1 段）
dshr --retention-plan
```

- 导出走 `crates/session-export`：先写 JSONL，再复制 spill 对象为附件，manifest 记录
  每份文件的 size/sha256；缺 blob 会记录在 `missing_blobs` 而不是静默丢。
- `--retention-plan` 只输出计划（plan/apply 分离），删除必须显式调用库 API。
- `--selftest-export` 覆盖导出 → 自校验 → JSONL 回放 → CLI 入口。

## 崩溃诊断

- panic hook 把时间、位置、消息、cwd 追加到 `DSHR_CRASH_LOG`（默认 `.tmp/crash.log`），
  再交给默认 hook（stderr 照常）；GUI 模式下可事后排查。
- sidecar 在装载阶段上报的 `log.emit(error)` 会显示在插件面板顶部（插件错误提示）。

## 边界

- 侧栏没有按时间分组、搜索、置顶（树顺序优先）；没有跨会话搜索/引用；切换期间不能生成。
- 重命名只改 meta；会话 ID 不变（外部引用/日志路径稳定）。
- 保留策略的附件字节未按会话归属（spill 对象跨会话去重共享），计划只统计会话 JSONL 段。
