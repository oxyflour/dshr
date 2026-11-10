# M2 状态对账（2026-11-11）

对照 `PLAN.md` 的 M2 四条 exit gate，逐条列证据与缺口。
结论：**M2 尚未完成**，缺口集中在官方写工具/服务图与真实性能测量。

## Gate 1：20 步 turn 工具调用成功率 ≥99%

**已有证据**

- 重试/退避（`docs/m2-stability.md`）：429/5xx/连接类指数退避，流式已交付后不重试；
- `--selftest-retry`：每第 3 次请求注入 503，20 轮、每轮一次工具调用；
  `succeeded=20 failed=0 tool_executions=20 provider_calls=59`；
- 严格工具协议（M1 A15）：`role: tool` + `tool_call_id` 配对，多工具轮次已验证；
- 审批拒绝/工具失败都会落成显式 tool 错误，不会静默丢。

**缺口**

- 没有真实网络/真实 provider 的 20 步成功率测量（需要 API key 与人工 live 测试）；
- 工具层瞬时错误（sidecar 超时/崩溃）没有重试分类，只有失败卡片；
- 没有按 step 统计成功率的持久化指标（`context.measured` 有，turn 成功率没有）。

## Gate 2：10k 事件冷启动 < 1.5s，滚动 FPS 有基线

**已有证据**

- `--selftest-perf`：10k 事件冷启动 **147ms**（load 119 + build 28），快照 30ms；
- UI 窗口化：只渲染最近 200 行 + "载入更早"，避免每帧上万元素；10k 会话截图可打开。

**缺口**

- 真实滚动 FPS 没有数字（窗口截图可抓但无法注入滚动/取帧）；A1c 的探针需要可交互桌面；
- 元素树内存未测；完整虚拟化（变高行）未恢复。

## Gate 3：压缩后上下文 ≤ 窗口 80%，回放/UI 一致

**已有证据**

- 计量 + 工具结果裁剪 + 超预算丢整轮（`docs/m2-context.md`）；
- LLM 摘要压缩 + `context.compacted`：`--selftest-context` 中 36814 → 3694 token
  （ratio 0.462；压缩后 0.610），压缩区间在 UI 显示「上下文已压缩 · seq a–b」，
  模型历史替换成 System 摘要，回放同规则；
- 压缩后计划不再重复丢弃（`dropped_messages == 0`）。

**缺口**

- 真实长会话（多次压缩叠加）未验证；摘要质量无评测集；
- spill 外部化（附件）未做。

## Gate 4：安装/启用/禁用普通 DSH 工具插件，不改插件源码

**已有证据**

- 官方 `dsh-tool-workspace-dependencies` 与只读 `dsh-tool-fs` 未改一行代码装载执行（M2 里程碑）；
- profile 启停面板；`--selftest-profile` 端到端：禁用 fs → `read` 消失，启用 → 回来；
- Loader YAML（bundle + patch 只读派生）；
- `chat --install-plugin <目录>` 离线安装（复制 + profile 条目 + 重启加载）。

**缺口**

- 安装只支持本地目录，没有 npm 网络解析/依赖树/签名审计；
- Loader 的 `include` / group 展开 / `!!js` 求值 / `isolate` 未实现；
- 官方写工具未解锁：需要 Node 侧 `fs` 写入实现、`sandbox`/`userApproval` service
  与 Rust↔Node 审批 RPC；这是"只用审批 UI"到"官方工具真正可用"的分界。

## 其他 M2 事项

| 项 | 状态 |
|---|---|
| 第二家 provider | ✅ DeepSeek + OpenAI 兼容 |
| 中文 IME | 组件可用，人工复验仍待做（issues/0001） |
| Markdown / 多会话 / UI 对齐 | ✅（见各 docs） |
| Session 树 / SQLite 索引 / 打包分发 | ❌ 未做 |

## 建议下一步

1. 官方写工具解锁（Node 侧 fs 写入 + sandbox/userApproval service + 审批 RPC）；
2. 真实 provider/工具失败矩阵与 20 步成功率统计；
3. 打包（Node 运行时 + sidecar 依赖）与干净环境启动。
