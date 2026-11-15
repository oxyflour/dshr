# Conformance Matrix（自动生成）

> 本文件由 `tools/conformance.mjs` 生成，请勿手改；测试清单事实源是 `docs/conformance.d/registry.json`。
> **CI 状态**：`.github/workflows/conformance.yml` 尚未在 GitHub Actions 实跑；本报告由本机命令生成。
> **格式**：compact（逐 ID 行只保留 ID/领域/状态/结果/证据/消息，title/notes 见注册表）。

## 元数据

| 字段 | 值 |
|---|---|
| generated_at | 2026-11-15T11:51:05.341Z |
| git HEAD | e10ba8f42c8fba1285dfeaf80716384684d54aae（dirty，3 个文件未提交） |
| git branch | master |
| platform | win32 10.0.26200 x64 |
| node | v24.14.0 |
| invocation | quick — `node tools/conformance.mjs --quick --compact --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md` |
| registry | docs/conformance.d/registry.json（schema_version=1，502 条测试） |

## 统计

| 指标 | 数量 |
|---|---:|
| 总测试 ID | 502 |
| implemented | 476 |
| implemented passed | 34 |
| implemented failed | 0 |
| implemented skipped | 442 |
| pending | 18 |
| manual | 8 |
| blocked | 0 |
| **failed（导致退出码 1）** | **0** |

## Suite 结果

| suite | 状态 | 退出码 | 耗时 | 命令 | 摘要 |
|---|---|---:|---:|---|---|
| schema | pass | 0 | 1.49s | `node schema/check.mjs` | 71/71 cases passed |
| links | pass | 0 | 0.36s | `node tools/check-links.mjs` | 318 link(s) checked, 0 broken, 61 file(s) scanned |
| node-test | pass | 0 | 0.28s | `node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs` | 13/13 node tests passed |

## 测试矩阵

| ID | 领域 | 状态 | 结果 | 证据 | 消息 |
|---|---|---|---|---|---|
| SCHEMA-FIXTURE-001 | SCHEMA | implemented | PASS | [element] valid: nested stack |  |
| SCHEMA-FIXTURE-002 | SCHEMA | implemented | PASS | [element] invalid: unknown element |  |
| SCHEMA-FIXTURE-003 | SCHEMA | implemented | PASS | [element] invalid: missing type |  |
| SCHEMA-FIXTURE-004 | SCHEMA | implemented | PASS | [element] invalid: child unknown type |  |
| SCHEMA-FIXTURE-005 | SCHEMA | implemented | PASS | [element] invalid: unknown top-level field |  |
| SCHEMA-FIXTURE-006 | SCHEMA | implemented | PASS | [slotRegistration] valid: panel |  |
| SCHEMA-FIXTURE-007 | SCHEMA | implemented | PASS | [slotRegistration] invalid: slot_id without dot |  |
| SCHEMA-FIXTURE-008 | SCHEMA | implemented | PASS | [slotRegistration] invalid: missing view |  |
| SCHEMA-FIXTURE-009 | SCHEMA | implemented | PASS | [slotRegistration] invalid: plugin_id uppercase |  |
| SCHEMA-FIXTURE-010 | SCHEMA | implemented | PASS | [slotRegistration] invalid: bad location |  |
| SCHEMA-FIXTURE-011 | SCHEMA | implemented | PASS | [manifest] valid: wasm tool plugin |  |
| SCHEMA-FIXTURE-012 | SCHEMA | implemented | PASS | [manifest] invalid: api v2 |  |
| SCHEMA-FIXTURE-013 | SCHEMA | implemented | PASS | [manifest] invalid: unknown runtime kind |  |
| SCHEMA-FIXTURE-014 | SCHEMA | implemented | PASS | [manifest] invalid: unknown top-level field |  |
| SCHEMA-FIXTURE-015 | SCHEMA | implemented | PASS | [manifest] invalid: memory over cap |  |
| SCHEMA-FIXTURE-016 | SCHEMA | implemented | PASS | [envelope] valid: tool.invoke |  |
| SCHEMA-FIXTURE-017 | SCHEMA | implemented | PASS | [envelope] invalid: missing trace_id |  |
| SCHEMA-FIXTURE-018 | SCHEMA | implemented | PASS | [envelope] invalid: depth over limit |  |
| SCHEMA-FIXTURE-019 | SCHEMA | implemented | PASS | [envelope] invalid: unknown kind |  |
| SCHEMA-FIXTURE-020 | SCHEMA | implemented | PASS | [envelope] invalid: bad error code |  |
| DOC-LINK-001 | DOC | implemented | PASS | all |  |
| CARGO-UNIT-001 | CARGO | implemented | SKIP | all |  |
| CARGO-UNIT-002 | CARGO | implemented | SKIP | dshr-plugin-host |  |
| CARGO-UNIT-003 | CARGO | implemented | SKIP | dshr-plugin-api |  |
| CARGO-UNIT-004 | CARGO | implemented | SKIP | dshr-ui-contributions |  |
| ADC-BIND-001 | ADC | pending | PENDING | — | 依赖 adc-v1 绑定/动作实现；断言见 review-m3-plan.md §6 ADC-BIND-*。 |
| ADC-ELEM-001 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-002 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-003 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-004 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-005 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-006 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-007 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-008 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-009 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-010 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-011 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-012 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-013 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-014 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-015 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-016 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-017 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-018 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-019 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-020 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-021 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-022 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-ELEM-A11Y-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-A11Y-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-A11Y-003 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-A11Y-004 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-ALIGN-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-CHART-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-CHART-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-GRID-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-GRID-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-ICON-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-JUSTIFY-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-LIST-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-NUM-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-003 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-004 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-005 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-006 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-007 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-008 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-009 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-010 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-PANEL-API-001 | ADC | implemented | SKIP | dshr-plugin-api |  |
| ADC-ELEM-PANEL-API-002 | ADC | implemented | SKIP | dshr-plugin-api |  |
| ADC-ELEM-PANEL-HOST-001 | ADC | implemented | SKIP | dshr-plugin-host |  |
| ADC-ELEM-PANEL-HOST-002 | ADC | implemented | SKIP | dshr-plugin-host |  |
| ADC-ELEM-PANEL-HOST-003 | ADC | implemented | SKIP | dshr-plugin-host |  |
| ADC-ELEM-PANEL-HOST-004 | ADC | implemented | SKIP | dshr-plugin-host |  |
| ADC-ELEM-PLACEHOLDER-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-RENDER-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-RENDER-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-RENDER-003 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-SLIDER-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-SPACING-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-SPACING-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-TABLE-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-TABLE-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-TEXT-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-TEXT-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-TEXT-003 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-ELEM-TONE-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-PANEL-001 | ADC | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-adc-panel：装载 fixture 插件→panels→render_panel（验证到 AnyElement 构造层）→未知元素 view；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
| ADC-PERF-001 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-PERF-002 | ADC | implemented | SKIP | dshr-ui-gpui |  |
| ADC-SEC-001 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-002 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-003 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-004 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-005 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-006 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-007 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-008 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SEC-009 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-001 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-002 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-003 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-004 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-005 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-006 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-007 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-008 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-009 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-010 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-011 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-012 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-013 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-014 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-015 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-016 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-017 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-018 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-019 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-020 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-021 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-022 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| ADC-SLOT-023 | ADC | implemented | SKIP | dshr-ui-contributions |  |
| CPP-AUTHOR-001 | CPP | manual | MANUAL | — | tools/authoring-check.ps1；仓库外按 docs/plugin-authoring.md + templates 构建/打包/安装（G2），人工/脚本判定。 |
| CPP-EFF-001 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-002 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-003 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-004 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-005 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-006 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-007 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-008 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-009 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-EFF-010 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-001 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-002 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-003 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-004 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-005 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-006 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-007 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-008 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-009 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-010 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-011 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-012 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-013 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-014 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-015 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-016 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-017 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-018 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-019 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-020 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-021 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-022 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-023 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-024 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-025 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-026 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-027 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-FIBER-101 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-102 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-103 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-104 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-105 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-111 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-112 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-113 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-114 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-115 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-116 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-121 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-122 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-123 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-124 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-125 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-126 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-127 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-131 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-132 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-FIBER-133 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-ISO-001 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-002 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-003 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-004 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-005 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-006 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-007 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-008 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-009 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-010 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-011 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-012 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-013 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-014 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-015 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-020 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-021 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-ISO-022 | CPP | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-external-plugin（安装→装载→调用→卸载→重载）；需构建 chat 二进制后运行，harness 尚无 selftest suite，人工/脚本判定。 |
| CPP-MANIFEST-001 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-002 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-003 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-004 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-005 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-006 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-007 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-008 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-009 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-010 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-011 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-012 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-013 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-014 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-015 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-016 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-017 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-018 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-019 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-020 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-021 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-022 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-023 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-024 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-025 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-MANIFEST-026 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-PROC-001 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-002 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-003 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-004 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-005 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-006 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-007 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-008 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-009 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-010 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-020 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-021 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-022 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-023 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-024 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-025 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-026 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-027 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-028 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-029 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-030 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-031 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-PROC-032 | CPP | implemented | SKIP | dshr-plugin-sdk |  |
| CPP-SVC-001 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-002 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-003 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-004 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-005 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-006 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-007 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-008 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-009 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-010 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-011 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-012 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-013 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-SVC-014 | CPP | implemented | SKIP | dshr-plugin-host |  |
| CPP-TRUST-001 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-002 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-003 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-004 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-005 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-006 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-007 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-008 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-009 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-010 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-011 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-012 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-013 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-014 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-015 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-016 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-017 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-018 | CPP | implemented | SKIP | dshr-profile |  |
| CPP-TRUST-019 | CPP | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-trust-install；需构建 chat 二进制后运行（harness 尚无 selftest suite），对应 M3 G2 信任链证据，人工/脚本判定。 |
| CPP-VER-001 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-VER-002 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-VER-003 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-VER-004 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-VER-005 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-VER-006 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-VER-007 | CPP | implemented | SKIP | dshr-plugin-api |  |
| CPP-WASM-001 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-002 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-003 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-004 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-005 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-006 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-007 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-008 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-009 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-010 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-011 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-012 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-013 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-014 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-015 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-016 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-017 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-018 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-019 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| CPP-WASM-020 | CPP | implemented | SKIP | dshr-plugin-wasm |  |
| RPC-L2-001 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-002 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-003 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-004 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-005 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-006 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-007 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-008 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-009 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-010 | RPC | implemented | SKIP | dshr-compat-dsh-protocol |  |
| RPC-L2-101 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-102 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-103 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-104 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-105 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-106 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-107 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-108 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-109 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-110 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-111 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-112 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-113 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-114 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-115 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-116 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-117 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-118 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-119 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-121 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-122 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-123 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-124 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-125 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-126 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-127 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-128 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-221 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-222 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-223 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-224 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-225 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-226 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-227 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-301 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-302 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-303 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-304 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-305 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-306 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-307 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-308 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-309 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-310 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-311 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-312 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-313 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-401 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-402 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-403 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-404 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-405 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-406 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-407 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-408 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-409 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-410 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-411 | RPC | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-l2：phase A 真实 sidecar protocol=2 握手+host_services，phase B fixture storage-kv 往返；需构建 chat 二进制（phase A 还需 vendored 官方 sidecar），harness 无 selftest suite，人工/脚本判定。 |
| RPC-L2-501 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-502 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-503 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-504 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| RPC-L2-505 | RPC | implemented | SKIP | dshr-compat-dsh-host |  |
| MCP-CLIENT-001 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-002 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-003 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-004 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-005 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-006 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-007 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-008 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-009 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-010 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-011 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-012 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-013 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-014 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-015 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-016 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| MCP-CLIENT-017 | MCP | implemented | SKIP | dshr-compat-mcp |  |
| JS-L2SVC-001 | JS | implemented | PASS | L2JS-001 |  |
| JS-L2SVC-002 | JS | implemented | PASS | L2JS-002 |  |
| JS-L2SVC-003 | JS | implemented | PASS | L2JS-003 |  |
| JS-L2SVC-004 | JS | implemented | PASS | L2JS-004 |  |
| JS-L2SVC-005 | JS | implemented | PASS | L2JS-005 |  |
| JS-L2SVC-006 | JS | implemented | PASS | L2JS-006 |  |
| JS-L2SVC-007 | JS | implemented | PASS | L2JS-007 |  |
| JS-L2SVC-008 | JS | implemented | PASS | L2JS-008 |  |
| JS-L2SVC-009 | JS | implemented | PASS | L2JS-009 |  |
| JS-L2SVC-010 | JS | implemented | PASS | L2JS-010 |  |
| JS-L2SVC-011 | JS | implemented | PASS | L2JS-011 |  |
| JS-L2SVC-012 | JS | implemented | PASS | L2JS-012 |  |
| JS-L2SVC-013 | JS | implemented | PASS | L2JS-013 |  |
| M3-DEFAULT-001 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-002 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-003 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-004 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-010 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-011 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-012 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-013 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-014 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-015 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-DEFAULT-020 | M3 | implemented | SKIP | dshr-llm-retry |  |
| M3-DEFAULT-021 | M3 | implemented | SKIP | dshr-llm-retry |  |
| M3-DEFAULT-022 | M3 | implemented | SKIP | dshr-llm-retry |  |
| M3-DEFAULT-023 | M3 | implemented | SKIP | dshr-llm-retry |  |
| M3-DEFAULT-024 | M3 | implemented | SKIP | dshr-llm-retry |  |
| M3-DEFAULT-025 | M3 | implemented | SKIP | dshr-llm-retry |  |
| M3-DEFAULT-030 | M3 | implemented | SKIP | dshr-llm-deepseek |  |
| M3-DEFAULT-031 | M3 | implemented | SKIP | dshr-llm-deepseek |  |
| M3-DEFAULT-032 | M3 | implemented | SKIP | dshr-llm-deepseek |  |
| M3-DEFAULT-035 | M3 | implemented | SKIP | dshr-llm-openai |  |
| M3-DEFAULT-036 | M3 | implemented | SKIP | dshr-llm-openai |  |
| M3-DEFAULT-037 | M3 | implemented | SKIP | dshr-llm-openai |  |
| M3-DEFAULT-040 | M3 | implemented | SKIP | dshr-agent-context |  |
| M3-DEFAULT-041 | M3 | implemented | SKIP | dshr-agent-context |  |
| M3-DEFAULT-042 | M3 | implemented | SKIP | dshr-agent-context |  |
| M3-DEFAULT-043 | M3 | implemented | SKIP | dshr-agent-context |  |
| M3-DEFAULT-044 | M3 | implemented | SKIP | dshr-agent-context |  |
| M3-DEFAULT-045 | M3 | implemented | SKIP | dshr-agent-context |  |
| M3-DEFAULT-050 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-051 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-052 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-053 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-054 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-055 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-056 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-060 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-061 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-062 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-063 | M3 | implemented | SKIP | dshr-tool-runtime |  |
| M3-DEFAULT-070 | M3 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-defaults：无网络自检 approval 默认/read-only/重复工具软提醒阈值/默认差异接线；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
| M3-LOADER-001 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-002 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-003 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-004 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-005 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-006 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-007 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-008 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-009 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-010 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-011 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-012 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-013 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-014 | M3 | implemented | SKIP | dshr-profile |  |
| M3-LOADER-015 | M3 | implemented | SKIP | dshr-profile |  |
| M3-PARALLEL-001 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-002 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-003 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-004 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-005 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-006 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-007 | M3 | implemented | SKIP | dshr-agent-core |  |
| M3-PARALLEL-101 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-102 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-103 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-104 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-105 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-106 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-107 | M3 | implemented | SKIP | dshr-compat-dsh-host |  |
| M3-PARALLEL-201 | M3 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-parallel-tools：sidecar 批 invoke 串行 6.1s→并行 2.0s（ratio 0.33）且事件顺序约束成立；需构建 chat 二进制 + vendored sidecar，harness 无 selftest suite，人工/脚本判定。 |
| SELF-CHAT-001 | SELF | pending | PENDING | — | 已存在但未纳入 harness；需构建 chat 二进制（未来 suite=selftest-chat）。 |
| SELF-CHAT-002 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-003 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-004 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-005 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-006 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-007 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-008 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-009 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-010 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-011 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-012 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-013 | SELF | pending | PENDING | — | 已存在但未纳入 harness；依赖本机 DSH 安装副本（CI 无此环境）。 |
| SELF-CHAT-014 | SELF | pending | PENDING | — | 已存在但未纳入 harness；依赖本机 DSH 安装副本。 |
| SELF-CHAT-015 | SELF | pending | PENDING | — | 已存在但未纳入 harness；依赖本机 DSH 安装副本。 |
| SELF-CHAT-016 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| SELF-CHAT-017 | SELF | pending | PENDING | — | 已存在但未纳入 harness；未来 suite=selftest-chat。 |
| GATE-G2-001 | GATE | manual | MANUAL | — | M3 G2 需人工 checklist：docs/plugin-authoring.md + templates + 未读 crates/ 源码；harness 无法自动判定。 |

## 说明与已知缺口

- 本轮批量条目用 groups 登记：harness 扫描 source_dirs 中的测试函数（函数名内编码或注释标注 ID），找不到对应测试函数的 ID 会直接报错，防止把未实现/无测试的 ID 标成 implemented。
- 本地 --workspace 实跑（2026-11-15，本机受限会话）：harness 用 `cargo test --offline --workspace --no-fail-fast` 跑完 66 suites / 796 passed / 1 failed；逐包判定 implemented 441 pass / 1 fail（仅 CARGO-UNIT-001 workspace-wide）/ 34 skip。唯一失败是 dshr-sidecar-guard 的 process::tests::drop_terminates_the_guarded_process（沙箱进程限制，隔离复跑同样失败，非本轮代码改动；docs/m3-status.md §4 已记录；普通终端/Lead 环境同一 workspace 为 66 suites / 797 passed / 0 failed），cargo 仅此 1 条环境失败，其余全绿。plain `cargo test --workspace`（不加 --no-fail-fast）会在该包失败后中止后续 target，故 harness 加 --no-fail-fast 以取得完整逐包证据。
- 第三轮新增证据：CPP-PROC（plugin-sdk，23）、CPP-ISO（plugin-host external_process，17）、CPP-MANIFEST-023..026（TOML，4）、MCP-CLIENT-001..017（compat-mcp）、RPC-L2-121..128/221..227（host services，15）均为 cargo implemented；sidecar JS 单测 13 个走新增 quick suite `node-test`（node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs），登记为 JS-L2SVC-001..013（源码 ID L2JS-xxx）。
- 第四轮新增证据：RPC-L2-401..410（compat-dsh-host tests/l2_host_services.rs，端到端 v2 握手/storage/wd/cancel/迟到/generation）、M3-LOADER-001..015（profile loader.rs include/group/patch/!!js）、ADC-ELEM-PANEL-001..010（ui-gpui adc.rs panel 入口）、ADC-PERF-001..002（ui-gpui tests/adc_perf.rs 预算，debug 约 4.4s）均为 cargo implemented；chat --selftest-l2 登记 RPC-L2-411 manual（phase A 真实 sidecar+phase B fixture storage-kv）。
- 第五轮新增证据：ADC-ELEM-PANEL-API-001..002（plugin-api）、ADC-ELEM-PANEL-HOST-001..004（plugin-host，含 external_process ui.render）、M3-DEFAULT-001..004/010..015/020..025/030..032/035..037/040..045/050..056/060..063（39 个，跨 agent-core/llm-*/agent-context/tool-runtime）、RPC-L2-501..505（compat-dsh-host tests/official_packages.rs，官方包装载/往返；依赖 .tmp/dsh-tools-all，缺失时测试内部 SKIP）均为 cargo implemented；chat --selftest-adc-panel 登记 ADC-PANEL-001 manual。
- 第六轮（W5/W6）新增证据：CPP-WASM-001..020（crates/plugin-wasm/tests/wasm_isolation.rs，wasmtime 主机 fuel/epoch/内存/输出/并发配额隔离 + core-module ABI；根 workspace 已纳入 dshr-plugin-wasm，不再 exclude）为 cargo implemented，selector=dshr-plugin-wasm；chat --selftest-defaults 登记 M3-DEFAULT-070 manual；docs/wasm-host.md、ADR-0010 更新、session-log tool.repeat 为文档证据（links 增长）。
- 第七轮（W7）新增证据：M3-PARALLEL-001..007（agent-core batch 调度：并发/顺序/预算截断/错误隔离/max_parallel/重复提醒/取消）、M3-PARALLEL-101..107（compat-dsh-host tests/parallel_batch.rs，sidecar 批 invoke 端到端）为 cargo implemented；chat --selftest-parallel-tools 登记 M3-PARALLEL-201 manual（串行 6.1s→并行 2.0s，ratio 0.33）；session-log 并行事件顺序约束等文档更新（links 变化）。
- ID 兼容说明：MCP 源码标注是两段式 MCP-001..017，按 spec/README §3 的 <AREA>-<CATEGORY>-<NNN> 规范登记为 MCP-CLIENT-001..017，title/notes 保留源码 ID；JS 同理（L2JS-001..013 → JS-L2SVC-001..013）。
- manual 证据：CPP-ISO-022 = chat --selftest-external-plugin（需构建 chat 二进制）、RPC-L2-411 = chat --selftest-l2（需 chat 二进制 + vendored sidecar）、ADC-PANEL-001 = chat --selftest-adc-panel（需 chat 二进制）、M3-DEFAULT-070 = chat --selftest-defaults（需 chat 二进制）、M3-PARALLEL-201 = chat --selftest-parallel-tools（需 chat 二进制 + vendored sidecar）、CPP-AUTHOR-001 = tools/authoring-check.ps1（需仓库外作者环境）、CPP-TRUST-019 = chat --selftest-trust-install；均按现有 schema 无法由 harness 自动判定，不计失败。
- 提交矩阵用 --quick --compact 生成：cargo suite 未在 quick 中运行，implemented 且 suite=cargo 的 ID 记为 SKIP；cargo 全绿待普通终端或 CI 复核。
- .github/workflows/conformance.yml 未在 GitHub Actions 实跑，矩阵由本机命令生成。
- compact 矩阵：markdown 保留逐 ID 的 ID/领域/状态/结果/证据/消息；JSON 的 tests.rows 为 [id, area, result]（status/证据/消息见 markdown 与注册表，result=pending|manual 即对应 status）。
- quick 模式不运行 cargo suite：implemented 且 suite=cargo 的 ID 显示 SKIP 属正常，需 --workspace 或 --full。
- cargo suite 在 Windows 上经 tools/build-env.cmd 调用（cargo test --offline --workspace --no-fail-fast）。
- .github/workflows/conformance.yml 未在 GitHub Actions 实跑，本报告由本机命令生成。
- 本文件由 tools/conformance.mjs 生成，请勿手改；新增证据请改 docs/conformance.d/registry.json。

退出码：`0`。pending / manual / blocked 不计失败。

