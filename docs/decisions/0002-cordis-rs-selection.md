# ADR-0002 — Rust Cordis 实现选型

- 状态：**Accepted**（2026-11-09 实测后决定）
- 日期：2026-11-08 立项，2026-11-09 实测
- 关联：PLAN ADR-002 / §3.5 / ADR-008
- 结论：**选 `jincdream/cordis-rs`**，`dshbox/cordis-rs` 转为并发/清理正确性的参考实现。

## 背景

CPP-v1 需要 Fiber / Service / Effect / Event 的 Rust 实现。两个候选都在 Windows 上构建并跑完了测试套件，因此本次不是"听 README 决定"，而是按六项门槛 + 依赖成本 + 语义贴合度做选择。

## 实测结果（Windows x64, MSVC 14.44, rustc 1.99.1）

| 项 | dshbox/cordis-rs | jincdream/cordis-rs |
|---|---|---|
| 完整 `cargo test` | **167 passed / 0 failed**（release，约 220 s 冷构建） | **111 passed / 0 failed**（约 55 s 冷构建，8.9 s 增量） |
| 代码规模 | 306 个 `.rs`，48,251 行 | 34 个 `.rs`，11,653 行 |
| crate 数 | 4（cordis / core / loader / timer） | 10（含 create / group / include / hmr / utils / logger-console） |
| 原生 C 依赖 | **有**：`alloca v0.4.0`（栈式协程，cc 编译） | **无** |
| license | MIT | MIT |
| 最近提交 | 2026-10-28（活跃，依赖 bump） | 2026-08-21（一次性完整实现） |
| 定位 | 类型化运行时 v3 契约（`inertia` / `era` 等自研术语），Cordis 只作灵感 | 对上游 `cordiverse/cordis` 冻结 commit 的**行为级移植**，163 个场景映射表 |

## 六项门槛的证据

| # | 场景 | dshbox 证据 | jincdream 证据 |
|---|---|---|---|
| T1 | Provider 失效后依赖 Fiber 重启 | `deps::reverse_projection_matches_fiber_owned_edges_and_unregisters`、`fiber::inertia::*` | `update_restarts_plugin_and_root_dispose_is_idempotent`、`reload_replaces_registered_services`、`inject_owns_method_effects_for_the_dependency_lifecycle` |
| T2 | 并发 apply / dispose | `fiber::caller_driven_owner_tests::*`、`fiber::era::*` | `loading_unloading_and_reloading_are_serialized`、`dropping_dispose_future_does_not_cancel_the_lifecycle_transition` |
| T3 | Effect 异步清理 | `effect::tests::*`（清理幂等、顺序、重入 8 项） | `effects_are_nested_reversed_idempotent_and_rolled_back_on_failure`、`disposer_failure_is_logged_and_does_not_reject_fiber_disposal` |
| T4 | 取消安全性 | `fiber::inertia::cancelling_*`、`effect::caller_driven_*` | `effect_setup_that_finishes_after_cancellation_rolls_back_its_resources`、`dropping_manual_effect_disposal_future_does_not_cancel_cleanup` |
| T5 | Service isolation | 有 `scope` 概念，测试以内部机制命名 | `isolated_services_and_shared_labels_are_independent` |
| T6 | 逆序注册撤销 | `effect::tests::remove_claims_the_occurrence_exactly_once` | `effects_are_nested_reversed_idempotent_...`（同一测试覆盖逆序） |

两套都通过，差异不在"能不能"，而在**语义距离**与**构建成本**。

## 决定

选 **`jincdream/cordis-rs`**，理由按权重排序：

1. **语义距离最短**：CPP-v1 定义为 Cordis 的严格子集，而它是行为级移植（含 `docs/upstream-scenario-evidence.md` 的场景→测试映射）。选 dshbox 意味着把 `inject / effect / service` 逐个翻译进它的 `inertia / era` 模型，这种翻译是长期维护债。
2. **无原生 C 依赖**：dshbox 的 `alloca` 依赖栈式协程，在 Windows 上要求 cc 工具链，并且会挡住未来把 `plugin-host` 编进 WASM/其他目标的路线。
3. **组件齐全**：loader / include / group / hmr / timer 都已提供，正好对应 CPP-v1 的 Config/Patch 与开发期热替换需求。
4. **构建快**：55 s vs 220 s（冷），对 CI 与迭代速度有实际影响。

同时保留 dshbox 作为**参考实现**：它的 effect 生命周期测试（重入、panic 传递、token 快照顺序）比 jincdream 更细，遇到边界问题时可对照它的测试写法补用例。

## 与 PLAN 默认的偏离（需知悉）

PLAN §3.5 的默认方向是"dshbox 优先、jincdream 对照"。本次据实测反转，理由如上。**PLAN §3.5 已同步更新**，避免文档与事实打架。

## 后果

- **好消息**：`plugin-host` 可以直接映射 Cordis 语义；构建链干净；测试套件可直接作为 conformance 基线。
- **坏消息**：jincdream 只有一次"feat: implement"提交（2026-08-21），单维护者、11.6k 行，成熟度低于 dshbox 的 48k 行；性能与边界情况需要我们自己压测。dshbox 的自研机制更完备，但我们不为它付翻译成本。

## 反悔条件

满足任一即回到评估（优先 dshbox，其次自研最小 Fiber）：

1. M1 结束前发现 jincdream 缺少 CPP-v1 必需能力（如 Config patch 语义、Service realm、异步 Effect 超时控制），且补丁超过 300 行；
2. 上游 Cordis 大版本演进导致冻结基线失去参考价值；
3. 压测显示 Fiber 切换/事件分发在我们目标场景下成为瓶颈（> 5% 帧时间）。

## 隔离要求（无论选谁）

`plugin-host` 是唯一允许依赖 `cordis-rs` 的 crate；其他 crate 只见 `plugin-api` 的 trait。这样换实现时只需重写 `plugin-host` 与它的 conformance 测试。

## 环境备注（实测踩坑，写入 runbook）

- 用户 `%TEMP%` 目录对本机链接器/编译器子进程不可写（`LNK1104: 无法打开文件 ...\Temp\lnk{GUID}.tmp`；`cc` 报 `D8050`）。把 `TEMP`/`TMP` 指到仓库内目录（如 `_probe\tmp`）后两个套件均正常。CI 必须设置该项，否则 Windows 构建会随机失败。
