# ADR-0010 — 隔离运行时配额模型与离线退路（WASM → external-process）

- 状态：Accepted（退路启用；WASM 主方案未取消）
- 日期：2026-11-14
- 关联：[`../PLAN.md`](../PLAN.md) §8 M3 G3、[`cpp-v1.md`](../spec/cpp-v1.md) §3.3/§3.5、
  [`0007-m3-scope.md`](0007-m3-scope.md) 反悔条件、[`../m3-status.md`](../m3-status.md)

## 背景

M3 的 G3 要求"WASM 在 fuel 耗尽/内存超限/超时三例分别被隔离，宿主存活、Session 无脏写"，
原计划是 Wasmtime + `cpp-v1.wit`（PLAN §3.3）。但当前开发机的网络被限制：`crates.io` 不可达，
本地 cargo 缓存与 `Cargo.lock` 里**没有** `wasmtime` / `cranelift` / WIT 相关 crate；
只有已锁定的依赖（serde/tokio/ring/gpui 等）可用。因此 M3 内无法按原方案引入 Wasmtime 主机。

ADR-0007 的反悔条件已经预留："WASM 若与线程模型冲突 → `external-process`（§3.3）作退路"。
本 ADR 把这条退路用于**本机离线**这一约束，并明确配额模型、验收口径与切换回 WASM 的条件。

## 选项

| 选项 | 内容 | 结论 |
|---|---|---|
| A. 等网络恢复再做 G3 | 最忠实原计划 | 否决：M3 其余部分会被一个外部依赖卡死，且无法给出证据 |
| B. Node `WebAssembly` + Worker | 无需新 crate | 否决：无 fuel/epoch、无 OS 级隔离（WASM 跑在 Node 进程内），达不到"超限隔离"的强度 |
| **C. `external-process` 隔离** | 复用 `sidecar-guard` 的 Windows Job Object（CPU/内存/子进程/随宿主退出）+ 独立数据目录 + 协议层配额 | **采用（M3 退路）** |
| D. 不做隔离，只靠声明 | —— | 否决：G3 名存实亡 |

## 决定

1. **配额五维在两种运行时里同构**。manifest 的 `runtime.fuel/max_memory_mb/timeout_ms` 语义映射：

   | 维度 | WASM 主机（主方案） | `external-process`（M3 退路） |
   |---|---|---|
   | 计算量 | Wasmtime fuel（`runtime.fuel`，上限 1e10） | Job Object CPU 时间上限（由 fuel 折算或 manifest 显式 `cpu_ms`） |
   | 墙钟超时 | epoch 中断（`timeout_ms`） | 宿主看门狗 + `TerminateJobObject`（同一 `timeout_ms`） |
   | 线性内存 | `max_memory_mb`（≤512） | Job Object 进程内存上限（同一字段） |
   | 输出 | 结果 1 MB 默认 / 8 MB 上限，超限转附件 | 管道字节计数上限，超限杀进程并按 `E_QUOTA` 结算 |
   | 并发 | 每插件并发 4 / 全局 16 | 每插件子进程数 + 并发调用数，同一上限 |

2. **`external-process` 的 ABI**：子进程用与 `sidecar-rpc` 相同的 NDJSON envelope（`version`、
   `request_id`、`generation`、`depth`、`trace_id`）走 stdio；`tool.invoke/result/chunk/cancel` 与
   `plugin.load/unload` 复用已有 kind，不新造协议。子进程崩溃 = generation 失效，在途调用 `aborted`，
   宿主不受影响。
3. **三种故障注入（G3 退路验收）**：
   - **超时**：插件不返回 → 看门狗超时 → 杀 Job → `E_QUOTA`（或 `E_TIMEOUT`）→ 宿主存活、Session 无脏写；
   - **内存**：插件持续分配 → Job 内存上限触发 → 进程被杀 → 同上；
   - **输出**：插件返回超大结果 → 管道/结果上限 → 截断或杀进程 → 结算 `E_QUOTA`，不得破坏会话。
   测试 ID 用 `CPP-ISO-001..`；WASM 主机就绪后同一批断言改跑 `CPP-WASM-*`，两套并存。
4. **WIT 与 WASM 主机不取消**：本轮仍交付 `spec/cpp-v1.wit`（世界定义：tool/effect/log/config +
   配额导入）与配额模型文档；Wasmtime 主机作为独立后续项（需要网络/vendored runtime）。
5. **诚实标注**：G3 的 M3 通过口径 = "隔离运行时（`external-process`）三故障注入 + 宿主存活"，
   **不是**"WASM Component 已交付"；m3-status 与最终 review 必须写明这一偏离及原因。

## 后果

**好消息**：G3 的"隔离"语义可以真正验证（Job Object 是 OS 级）；不需要网络；复用 M2 已测试的
`sidecar-guard`，风险低；协议复用 sidecar envelope，未来 WASM 主机可复用同一配额/结算模型。

**坏消息**：外部进程比 WASM 重（启动/内存），单次调用成本更高；`external-process` 插件要按我们的
envelope 写适配层，不能直接跑任意 npm 插件；WASM 的"零信任轻量分发"卖点推迟到网络/运行时可用后。

## 反悔条件

- 一旦 `wasmtime`（或等价 vendored runtime）可获取：实现 WASM 主机，G3 主口径切回 `CPP-WASM-*`，
  `external-process` 保留为高权限/高成本插件的第二通道；
- 若 Job Object 在目标 Windows 版本上不可用：退到"独立用户 + 受限 token"（PLAN §7），验收口径不变；
- 若进程隔离仍无法满足超时精度（<100ms）：放宽到"看门狗 100ms 粒度"并记录，不得静默接受超时失控。

## 验证

- `CPP-ISO-001..`：超时/内存/输出三故障注入各自隔离、宿主存活、Session 无脏写；
- `CPP-ISO-010..`：正常外部进程插件 load → tool.invoke → 结果 → unload → 无残留进程/句柄；
- 进程计数器（`tasklist`/Job 查询）证明杀干净；宿主退出时 Job 清理子进程；
- conformance 矩阵中 `CPP-ISO-*` 为 implemented，`CPP-WASM-*` 保持 pending 并注明"等待 wasmtime"。
