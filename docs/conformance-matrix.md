# Conformance Matrix（自动生成）

> 本文件由 `tools/conformance.mjs` 生成，请勿手改；测试清单事实源是 `docs/conformance.d/registry.json`。
> **CI 状态**：`.github/workflows/conformance.yml` 尚未在 GitHub Actions 实跑；本报告由本机命令生成。

## 元数据

| 字段 | 值 |
|---|---|
| generated_at | 2026-11-12T14:55:07.202Z |
| git HEAD | c688ec8de306d3b18987eddfc511a11425b31477（dirty，28 个文件未提交） |
| git branch | master |
| platform | win32 10.0.26200 x64 |
| node | v24.14.0 |
| invocation | quick — `node tools/conformance.mjs --quick --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md` |
| registry | docs/conformance.d/registry.json（schema_version=1，57 条测试） |

## 统计

| 指标 | 数量 |
|---|---:|
| 总测试 ID | 57 |
| implemented | 25 |
| implemented passed | 21 |
| implemented failed | 0 |
| implemented skipped | 4 |
| pending | 31 |
| manual | 1 |
| blocked | 0 |
| **failed（导致退出码 1）** | **0** |

## Suite 结果

| suite | 状态 | 退出码 | 耗时 | 命令 | 摘要 |
|---|---|---:|---:|---|---|
| schema | pass | 0 | 0.21s | `node schema/check.mjs` | 65/65 cases passed |
| links | pass | 0 | 0.22s | `node tools/check-links.mjs` | 228 link(s) checked, 0 broken, 51 file(s) scanned |

## 测试矩阵

| ID | 领域 | 标题 | 状态 | 结果 | 证据 | 消息 |
|---|---|---|---|---|---|---|
| SCHEMA-FIXTURE-001 | SCHEMA | element: valid nested stack | implemented | PASS | schema: [element] valid: nested stack | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-002 | SCHEMA | element: invalid unknown element | implemented | PASS | schema: [element] invalid: unknown element | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-003 | SCHEMA | element: invalid missing type | implemented | PASS | schema: [element] invalid: missing type | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-004 | SCHEMA | element: invalid child unknown type | implemented | PASS | schema: [element] invalid: child unknown type | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-005 | SCHEMA | element: invalid unknown top-level field | implemented | PASS | schema: [element] invalid: unknown top-level field | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-006 | SCHEMA | slotRegistration: valid panel | implemented | PASS | schema: [slotRegistration] valid: panel | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-007 | SCHEMA | slotRegistration: invalid slot_id without dot | implemented | PASS | schema: [slotRegistration] invalid: slot_id without dot | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-008 | SCHEMA | slotRegistration: invalid missing view | implemented | PASS | schema: [slotRegistration] invalid: missing view | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-009 | SCHEMA | slotRegistration: invalid plugin_id uppercase | implemented | PASS | schema: [slotRegistration] invalid: plugin_id uppercase | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-010 | SCHEMA | slotRegistration: invalid bad location | implemented | PASS | schema: [slotRegistration] invalid: bad location | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-011 | SCHEMA | manifest: valid wasm tool plugin | implemented | PASS | schema: [manifest] valid: wasm tool plugin | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-012 | SCHEMA | manifest: invalid api v2 | implemented | PASS | schema: [manifest] invalid: api v2 | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-013 | SCHEMA | manifest: invalid unknown runtime kind | implemented | PASS | schema: [manifest] invalid: unknown runtime kind | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-014 | SCHEMA | manifest: invalid unknown top-level field | implemented | PASS | schema: [manifest] invalid: unknown top-level field | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-015 | SCHEMA | manifest: invalid memory over cap | implemented | PASS | schema: [manifest] invalid: memory over cap | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-016 | SCHEMA | envelope: valid tool.invoke | implemented | PASS | schema: [envelope] valid: tool.invoke | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-017 | SCHEMA | envelope: invalid missing trace_id | implemented | PASS | schema: [envelope] invalid: missing trace_id | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-018 | SCHEMA | envelope: invalid depth over limit | implemented | PASS | schema: [envelope] invalid: depth over limit | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-019 | SCHEMA | envelope: invalid unknown kind | implemented | PASS | schema: [envelope] invalid: unknown kind | suite 通过（65/65 cases passed） |
| SCHEMA-FIXTURE-020 | SCHEMA | envelope: invalid bad error code | implemented | PASS | schema: [envelope] invalid: bad error code | suite 通过（65/65 cases passed） |
| DOC-LINK-001 | DOC | 全部 Markdown 相对链接可解析 | implemented | PASS | links: all | 链接检查通过（228 link(s) checked, 0 broken, 51 file(s) scanned） |
| CARGO-UNIT-001 | CARGO | Rust workspace 全部单测 | implemented | SKIP | cargo: all | suite "cargo" 未在本次调用中运行（用 --suite cargo / --workspace / --full 运行） |
| CARGO-UNIT-002 | CARGO | dshr-plugin-host 单测 | implemented | SKIP | cargo: dshr-plugin-host | suite "cargo" 未在本次调用中运行（用 --suite cargo / --workspace / --full 运行） |
| CARGO-UNIT-003 | CARGO | dshr-plugin-api 单测 | implemented | SKIP | cargo: dshr-plugin-api | suite "cargo" 未在本次调用中运行（用 --suite cargo / --workspace / --full 运行） |
| CARGO-UNIT-004 | CARGO | dshr-ui-contributions 单测 | implemented | SKIP | cargo: dshr-ui-contributions | suite "cargo" 未在本次调用中运行（用 --suite cargo / --workspace / --full 运行） |
| ADC-SLOT-001 | ADC | 12 个 slot 的 kind/scope/注册契约 | pending | PENDING | — | 依赖 docs/spec/adc-v1.md §5 冻结 + schema/adc-v1/slots.v1.json；当前 ui-contributions 无 slot 服务实现。 |
| ADC-ELEM-001 | ADC | 26 元素渲染快照 + 未知 type → Placeholder | pending | PENDING | — | 依赖 ui-gpui 渲染器；未知元素降级为 M3 G1 核心断言（review-m3-plan.md §4）。 |
| ADC-BIND-001 | ADC | 绑定前缀权限与 patch seq 乱序丢弃 | pending | PENDING | — | 依赖 adc-v1 绑定/动作实现；断言见 review-m3-plan.md §6 ADC-BIND-*。 |
| ADC-SEC-001 | ADC | openExternal 仅 https、跨插件绑定拒绝 | pending | PENDING | — | 依赖 ADC 服务端与动作执行器；断言见 review-m3-plan.md §6 ADC-SEC-*。 |
| CPP-MANIFEST-001 | CPP | manifest 未知字段/min_host/版本检查 | pending | PENDING | — | schema 侧已有 SCHEMA-FIXTURE-011..015；运行期 typed Manifest 解析器尚未实现。 |
| CPP-VER-001 | CPP | api/WIT/min_host 版本不匹配拒绝 | pending | PENDING | — | 依赖 plugin-api/plugin-host 版本检查；当前 crate 版本 0.0.0。 |
| CPP-FIBER-001 | CPP | Fiber 状态机与依赖重启 | pending | PENDING | — | 依赖 cordis-rs 接入（ADR-0002 未落地）。 |
| CPP-SVC-001 | CPP | Service 失效/恢复 | pending | PENDING | — | 依赖 Fiber/Service 依赖图实现。 |
| CPP-EFF-001 | CPP | Effect LIFO/幂等/超时 | pending | PENDING | — | 依赖 Effect 撤销语义实现（plugin-host 当前不含 Effect 撤销）。 |
| CPP-WASM-001 | CPP | fuel/内存/超时三例分别隔离 | pending | PENDING | — | 依赖 spec/cpp-v1.wit + Wasmtime host + 配额 ADR-0010；M3 G3 证据。 |
| CPP-TRUST-001 | CPP | 篡改包/降级拒绝/审计字段 | pending | PENDING | — | 依赖 spec/plugin-trust.md + 验签钩子；当前安装不校验签名。 |
| RPC-L2-001 | RPC | L2 storage 服务往返 | pending | PENDING | — | 依赖 sidecar-rpc v2 host-service 代理 + storage adapter；M3 按证据选服务。 |
| RPC-L2-002 | RPC | L2 workingDirectory 服务往返 | pending | PENDING | — | 候选服务见 review-m3-plan.md §2.1；需真实官方插件证据。 |
| RPC-L2-003 | RPC | L2 sessionProjections 服务往返 | pending | PENDING | — | 候选服务见 review-m3-plan.md §2.1；需真实官方插件证据。 |
| SELF-CHAT-001 | SELF | chat --selftest 基本对话 | pending | PENDING | — | 已存在但未纳入 harness；需构建 chat 二进制（未来 suite=selftest-chat）。 |
| SELF-CHAT-002 | SELF | chat --selftest-stream 流式 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-003 | SELF | chat --selftest-context 上下文计量 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-004 | SELF | chat --selftest-long-context 长上下文压缩 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-005 | SELF | chat --selftest-spill 外部化 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-006 | SELF | chat --selftest-approval 审批 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-007 | SELF | chat --selftest-sandbox 沙箱 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-008 | SELF | chat --selftest-profile 插件启停 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-009 | SELF | chat --selftest-loader Loader 派生 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-010 | SELF | chat --selftest-install 离线安装 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-011 | SELF | chat --selftest-retry 重试 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-012 | SELF | chat --selftest-perf 性能 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-013 | SELF | chat --selftest-dsh DSH L1 | pending | PENDING | — | 已存在但未纳入 harness；依赖本机 DSH 安装副本（CI 无此环境）。 |
| SELF-CHAT-014 | SELF | chat --selftest-dsh-write 官方写路径 | pending | PENDING | — | 已存在但未纳入 harness；依赖本机 DSH 安装副本。 |
| SELF-CHAT-015 | SELF | chat --selftest-cordis Cordis 插件 | pending | PENDING | — | 已存在但未纳入 harness；依赖本机 DSH 安装副本。 |
| SELF-CHAT-016 | SELF | chat --selftest-tree 会话树 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-017 | SELF | chat --selftest-export 会话导出 | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| GATE-G2-001 | GATE | 外部作者可发布实验（仓库外构建 + 安装 + 卸载） | manual | MANUAL | — | M3 G2 需人工 checklist：docs/plugin-authoring.md + templates + 未读 crates/ 源码；harness 无法自动判定。 |

## 说明与已知缺口

- 本地 --workspace 实跑记录（2026-11-12，本机受限会话）：cargo suite 26 suites / 316 passed / 1 failed；唯一失败是 dshr-sidecar-guard 的 process::tests::drop_terminates_the_guarded_process（隔离复跑同样失败，属沙箱进程限制，非本轮代码改动；docs/m3-status.md §4 已记录）。提交矩阵用 --quick 生成，CARGO-UNIT-* 记为 SKIP，cargo 全绿待普通终端或 CI 复核。
- quick 模式不运行 cargo suite：CARGO-UNIT-* 显示 SKIP 属正常，需 --workspace 或 --full。
- cargo suite 在 Windows 上经 tools/build-env.cmd 调用（cargo test --offline --workspace）。
- .github/workflows/conformance.yml 未在 GitHub Actions 实跑，本报告由本机命令生成。
- 本文件由 tools/conformance.mjs 生成，请勿手改；新增证据请改 docs/conformance.d/registry.json。

退出码：`0`。pending / manual / blocked 不计失败。

