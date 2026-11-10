# 性能基线（F，第二阶段，2026-11-11）

M2 gate 2 要求：10k 事件冷启动 < 1.5s，并有滚动/渲染的可重复基线。
这一轮先把**数据层**测出来，并把 UI 从"每帧构造整段历史"改成**窗口化渲染**。

## 数据层数字（本机，debug build）

`chat --selftest-perf` 构造 10,000 条事件（2,500 轮 × user/assistant/tool.call/tool.result）：

```text
[perf] events=10000 jsonl_bytes=2619734 load_ms=119 build_ms=28
       snapshot_ms=30 history_ms=26 cold_ms=147
       items=7500 rows=7500 history_messages=60
```

| 项 | 结果 |
|---|---|
| JSONL 大小 | 2.62 MB |
| `SessionLog::from_jsonl`（含全部不变量重放） | 119 ms |
| `build_from_log`（对话模型） | 28 ms |
| `rows_snapshot`（UI 快照） | 30 ms |
| 冷启动合计（load + build） | **147 ms**，目标 < 1500 ms |
| `history_for_model`（上下文重建 + 60 条截断） | 26 ms |

结论：数据层不是瓶颈，10k 事件有约 10 倍余量。

## UI：窗口化渲染

之前 transcript 每帧为**全部** rows 构造元素；10k 事件（7.5k 行）在 debug 下每帧
要构造上万元素，滚动会明显卡。现在：

- 默认只渲染最近 **200 行**；
- 顶部保留"载入更早的 N 条（每次 200 条）"入口，点一次可见范围 +200；
- 切换/新建会话时重置为 200；
- 消息总数仍显示在头部（如 `7500 条`）。

代价：滚动条不再代表完整历史（这是有意的权衡）；真正的虚拟化（`uniform_list`）
需要恢复变高行支持，留给后续。

## 未做 / 待人工验证

- **真实 FPS 数字**：本机 GPUI 窗口只能 `PrintWindow` 截图，无法注入稳定滚动与
  取帧时间；当前只能证明"渲染行数有上限"（截图：10k 会话打开、只渲染尾部）。
  A1/A1c 的探针可复用到真实窗口，但需要可交互的桌面会话。
- 长会话的**内存**（元素树/文本 shaping 缓存）未测。
- 缩略/虚拟化的完整方案（变高行 + 锚点恢复）未做。
