# M2 收尾计划（2026-11-11）

目标：把 [`m2-status.md`](m2-status.md) 列出的缺口收掉，让 `PLAN.md` §8 的 M2 四条 exit gate
全部有**可复现命令 + 数字**，并把 M2 清单项落地（除明确"可后移"的 ACP/MCP）。

## 工作流划分

| 流 | 内容 | 主要文件 | 负责人 |
|---|---|---|---|
| W1 | 会话树：`parent_session_id` / `spawn_source` / spawn_child / 中断传播 / drain / 树查询 | `crates/session-store`、`crates/agent-core`、`docs/spec/session-log.md` | T1 |
| W2 | 上下文外部化（spill）+ 会话导出/保留策略 | `crates/agent-context`、新 crate `crates/session-export` | T2 |
| W3 | 官方写工具解锁：Node `fs` 写入 + `sandbox` seam + Rust↔Node 审批 RPC | `sidecar/dsh-official/`、`crates/compat-dsh-host`、`crates/compat-dsh-protocol`、新 crate `crates/compat-dsh-fs` | T3 |
| W4 | Node 运行时分发（捆绑/离线 vendor）+ Sidecar Job Object 隔离 + npm 包安装 | 新 crate `crates/sidecar-guard`、`crates/profile/src/install.rs`、`tools/` | T4 |
| W5 | Diff / Terminal 原生视图组件 | `crates/ui-gpui/src/diff.rs`、`terminal.rs`、`lib.rs` | T5 |
| W6 | 帧耗时探针 + 滚动 FPS 测量（Gate 2） | `apps/desktop-gpui/src/bin/chat.rs`、`tools/screen.ps1` | Lead |
| W7 | 长会话多次压缩自检（Gate 3） | `chat.rs --selftest-long-context` | Lead |
| W8 | 集成、文档、提交、最终对账 | 全仓 | Lead |

**写域约束**：除 Lead 外任何人不得改 `apps/desktop-gpui/src/bin/chat.rs`；跨 crate 的
UI/接线由 Lead 在集成轮完成。Teammate 改动不提交，由 Lead 统一 review + commit。

## 退出条件映射

### Gate 1 — 20 步工具调用成功率 ≥99%

- 已有：故障注入 20 轮全成功、重试退避、严格工具协议。
- 补齐：真实 provider 的 20 步 live 自检（`--selftest-live-20`），逐 step 记录
  `provider_calls / tool_calls / failures`，落 `docs/m2-stability.md`。
- 补齐：sidecar 侧瞬时错误（超时/崩溃）的分类与重试建议（T3 顺带产出）。

### Gate 2 — 10k 事件冷启动 < 1.5s；滚动 FPS 有基线

- 已有：`--selftest-perf` 冷启动 145–147ms。
- 补齐：帧耗时探针（`DSHR_FRAME_LOG`），用 `PostMessage(WM_MOUSEWHEEL)` 注入滚动，
  计算 P50/P95 帧间隔与 FPS；与基线（本机首测值）比较并写进 `docs/m2-performance.md`。
- 补齐：10k 会话滚动时的元素数量/内存估计（日志打点）。

### Gate 3 — 压缩后上下文 ≤ 窗口 80%，回放与 UI 一致

- 已有：单次压缩 36814 → 3694 token（0.462），回放同规则。
- 补齐：`--selftest-long-context`：mock provider 产生超长工具结果，连续 5+ 次压缩叠加，
  每次断言 `tokens_after/window ≤ 0.8`、`dropped_messages == 0`、回放历史与 UI 行数一致。

### Gate 4 — 安装/启用/禁用普通 DSH Tool 插件且不改源码

- 已有：官方 read-only fs 装载、profile 启停、Loader YAML、目录安装。
- 补齐：`--install-plugin` 支持 npm 包 tarball（`.tgz`，离线解包）；Loader
  `include` 展开与 group 展开（T4）；官方 `write`/`edit` 工具在审批 + 沙箱下真正可用（T3）。
- 验收：安装一个官方 tarball → profile 出现条目 → 禁用后工具消失 → 启用后恢复 →
  写工具在 Deny 模式下被拒、Allow 模式下经审批落盘（并留 sandbox 快照）。

## 明确后移

| 项 | 原因 |
|---|---|
| MCP / ACP adapter | `PLAN.md` §8 M2 转向允许后移 M3 |
| WASM / 动态原生插件 | M3 |
| 完整变高虚拟化 | Gate 2 只需要可测 FPS 基线与窗口化，元素树计数已足够 |
| DSH L2 `storage`/`settings` | M3 首批；M2 仅需 `fs`/`sandbox`/`userApproval`（W3） |

## 验收命令（最终轮全跑）

```powershell
tools\build-env.cmd cargo test --workspace
tools\build-env.cmd cargo clippy --workspace --all-targets -- -D warnings
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest
# + 每个 feature selftest：stream / context / long-context / approval / sandbox /
#   profile / loader / install / retry / perf / dsh / dsh-write / tree / frame
```
