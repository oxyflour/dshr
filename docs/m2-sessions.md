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

- 每个会话对应一份 `ChatState`（Agent + 插件 + 侧车 + 日志路径）。
  切换 = 停掉旧状态、按新会话路径新建一份，再整体替换 UI 快照。
- **生成中禁止切换**：工作线程还在往旧 state 写，切走会让 UI 与日志分叉。
- 切换会重新启动 sidecar / 加载插件（每会话独立），这是当前实现的代价；
  后续可以把 `SidecarHost` 提升为进程级共享，只按会话换 Agent/日志。

## UI

- 侧栏：标题 + "N 条 · 相对时间"，当前会话高亮；`✎` 进入行内重命名（✓ 提交 / ✕ 取消），
  `×` 进入二次确认（删除 / 取消），避免误删。
- `＋ 新建对话`：生成 `s-<毫秒>` 并切过去。
- 头部面包屑显示当前会话标题（长标题截断到 220 逻辑像素，避免和右侧状态挤在一起）。
- 新会话给的是空态页（DSH 的"开始对话"引导）。

## 边界

- 侧栏没有按时间分组、搜索、置顶；列表只按 mtime 倒序。
- 没有跨会话搜索/引用；切换期间不能生成。
- 每个会话独立 sidecar：会话多、频繁切换会有进程启动开销。
- 重命名只改 meta；会话 ID 不变（外部引用/日志路径稳定）。
