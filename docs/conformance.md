# Conformance 门禁（测试 ID 注册表 + 矩阵报告）

> 目标：兑现 [`PLAN.md`](PLAN.md) §10.1「conformance matrix 是 CI 产物，不是文档表格」。
> 本文说明怎么加测试 ID、怎么跑、当前矩阵与已知缺口；机器可读报告是
> [`conformance-matrix.json`](conformance-matrix.json)，人读表格是
> [`conformance-matrix.md`](conformance-matrix.md)。

## 1. 组成

| 文件 | 作用 |
|---|---|
| [`tools/conformance.mjs`](../tools/conformance.mjs) | harness：校验注册表、执行 suite、按 selector 判定、生成矩阵报告 |
| [`conformance.d/registry.json`](conformance.d/registry.json) | **唯一事实源**：suite 定义、显式测试 ID，以及本轮起使用的 `groups` 批量条目（id/area/status/suite/selector/notes） |
| [`conformance-matrix.json`](conformance-matrix.json) | 机器读矩阵（generated_at / git HEAD / platform / suite 结果 / 逐 ID 行） |
| [`conformance-matrix.md`](conformance-matrix.md) | 人读矩阵（人读表格，CI artifact 也上传这份） |
| [`.github/workflows/conformance.yml`](../.github/workflows/conformance.yml) | Windows CI：契约校验 + Rust 全量单测 + 矩阵 artifact |

harness 是 Node ESM、零 npm 依赖、Node ≥ 20，Windows / macOS / Linux 均可运行。
进程输出通过临时文件（`.tmp/conformance/`）捕获，不依赖管道，因此在受限沙箱里也能跑。

## 2. 测试 ID 规范

沿用 [`spec/README.md`](spec/README.md) §3：

```text
<AREA>-<CATEGORY>-<NNN>     例如 RPC-CANCEL-003、ADC-SLOT-011、SCHEMA-FIXTURE-001
```

- `AREA`：测试领域（`SCHEMA`、`DOC`、`CARGO`、`ADC`、`CPP`、`RPC`、`MCP`、`JS`、`SELF`、`GATE`）。
- `CATEGORY`：断言分组（`FIXTURE`、`LINK`、`UNIT`、`SLOT`、`ELEM`、`L2` …）；允许再嵌一段，
  如 `ADC-ELEM-SPACING-001`（ui-gpui 渲染器）。
- `NNN`：三位数字，在 `AREA-CATEGORY` 内唯一、不复用。
- 注册表里 `area` 字段必须与 ID 首段一致，ID 全库唯一。

### 2.1 批量条目（`groups`）

本轮新增 200+ 条已实现测试，逐条手写 `tests` 既冗长又易与源码脱节。`groups` 用一个组声明
`source_dirs` + `ids`：

```json
{
  "name": "ADC-SLOT（ui-contributions：slot 目录/注册表/委托）",
  "area": "ADC", "status": "implemented", "suite": "cargo",
  "selector": "dshr-ui-contributions",
  "notes": "crates/ui-contributions/tests/adc_slot.rs；cargo test -p dshr-ui-contributions",
  "source_dirs": ["crates/ui-contributions/tests"],
  "ids": ["ADC-SLOT-001", "ADC-SLOT-002", "..."]
}
```

harness 会扫描 `source_dirs` 下的 `.rs`，从测试函数名（`adc_slot_001_*` → `ADC-SLOT-001`）
或注释标注（`// CPP-TRUST-001：...`）建立 ID → 测试函数映射；`ids` 里任何一条找不到对应
测试函数都会**注册表校验失败**。展开后的条目与显式 `tests` 同等参与唯一性/格式/可执行来源校验，
title 取测试函数名，notes 追加来源文件。这同时满足「ID 归属哪个 crate 必须可核查」：
组里的 `selector` 就是该 crate 的 cargo 包名。

## 3. 状态机

`status` 是**注册表里的承诺状态**；`result` 是**本次运行的结果**（由 harness 计算）。

| status | 含义 | 是否影响退出码 |
|---|---|---|
| `implemented` | 已有可执行证据：必须声明 `suite` + `selector`，且 selector 在本次运行中命中；`groups` 里还必须能在 `source_dirs` 找到对应测试函数 | 是（未命中/失败 → FAIL） |
| `pending` | 已定义断言但实现/契约未就绪（`ADC-BIND-001`、17 个 chat selftest） | 否 |
| `manual` | 需人工/脚本实验判定（`GATE-G2-001`、`CPP-TRUST-019`、`CPP-ISO-022`、`RPC-L2-411`、`ADC-PANEL-001`、`M3-DEFAULT-070`、`M3-PARALLEL-201`、`CPP-AUTHOR-001`） | 否 |
| `blocked` | 被外部依赖阻塞（当前暂无条目；语义保留给前置未落地时） | 否 |

`result` 取值：`PASS` / `FAIL` / `SKIP`（suite 本次未选中，例如 `--quick` 下 `suite=cargo` 的 ID）/
`PENDING` / `MANUAL` / `BLOCKED`。

规则：

- `implemented` 但没有 `suite` + `selector` → **注册表校验失败**（退出码 1）。
- `pending` / `manual` / `blocked` 不得声明 `suite` 或 `selector`，防止把未实现的东西伪装成可执行证据。
- `--quick` 不跑 cargo：所有 `suite=cargo` 的 implemented ID 显示 SKIP 是正常状态，不是失败；
  要跑用 `--workspace` 或 `--full`。
- 退出码：0 = 通过；1 = 有失败项或注册表非法；2 = 用法错误。pending/manual/blocked 永不导致失败。

## 4. suite

| suite | 命令 | quick | 默认超时 | selector 语义 |
|---|---|---|---|---|
| `schema` | `node schema/check.mjs` | 是 | 120s | 输出中 `ok [schema] label` 行；selector 取 `[schema] label` |
| `links` | `node tools/check-links.mjs` | 是 | 120s | 只有 `all`：suite 整体通过即满足 |
| `node-test` | `node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs` | 是 | 120s | 从 `✔ name` / TAP `ok N - name` 行匹配 selector（如 `L2JS-001`），并要求输出汇总 `pass`/`fail` 与退出码一致 |
| `cargo` | `tools\build-env.cmd cargo test --offline --workspace --no-fail-fast`（Windows；其它平台同样加 `--no-fail-fast`） | 否 | 1200s（20 min） | `all` 要求 workspace 整体通过；包名（如 `dshr-plugin-host`）在该包测试二进制出现、且该包不是失败包时通过——workspace 里其它包的环境失败不会连坐本包。`--no-fail-fast` 保证一个包失败后其余包仍会运行（否则 cargo 中止、后面的包无法判定） |

Windows 上 cargo 必须经 [`tools/build-env.cmd`](../tools/build-env.cmd)（本机 `%TEMP%` 不可写，
见 [`m0-runbook.md`](m0-runbook.md)）；harness 用 `cmd /c` 调用 `.cmd`。cargo 超时可用
`--timeout-ms` 覆盖。

## 5. 命令

```powershell
# 秒级门禁：schema + links + node-test + 注册表校验
node tools/conformance.mjs --quick

# 生成矩阵（提交进仓库的两份文件；370+ 条 ID 用 --compact 控制体积）
node tools/conformance.mjs --quick --compact --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md

# 查看注册表（不执行；会扫描 groups 的 source_dirs 验证 ID 有测试来源）
node tools/conformance.mjs --list

# 只跑某个 suite（可重复）
node tools/conformance.mjs --suite schema --suite links

# 仅校验注册表 candidate（不落盘）
node tools/conformance.mjs --registry .tmp/registry-candidate.json --list

# 额外跑 Rust 全量单测（默认不跑；约数分钟）
node tools/conformance.mjs --workspace

# 全部（生成含 cargo 结果的完整矩阵）
node tools/conformance.mjs --full --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md
```

参数：`--compact` 矩阵紧凑模式（逐 ID 行去掉 title/notes 与 PASS 行重复消息）；
`--registry <path>` 换用其它注册表文件；`--timeout-ms <n>` 覆盖本次所有 suite 超时；
`--json`/`--markdown` 后接路径可改输出位置（缺省用 `docs/conformance-matrix.*`）；
`--help` 查看用法。

> 报告写入失败时（例如目标目录被沙箱设为只读），harness 会把报告回退写入
> `.tmp/conformance/` 并打印警告，退出码仍由测试结果决定；CI 由 artifact 上传步骤
> （`if-no-files-found: error`）兜底。此时请用有写权限的会话重跑，或用 DSH 文件工具把回退文件复制到目标路径。

## 6. 当前矩阵摘要

第七轮（W7 工具并行收口，2026-11-15 生成）：**共 502 个测试 ID —— implemented 476、
pending 18、manual 8、failed 0（quick）**。`--quick --compact` 下 34 通过（20 schema fixture +
1 doc link + 13 个 `node-test` JS 单测），442 个 `suite=cargo` 的 implemented ID 记为 SKIP
（quick 不构建 Rust）。矩阵报告带 `generated_at`、`git HEAD`（含 dirty 文件数）、platform、
逐 suite 结果与逐 ID 行；以 [`conformance-matrix.json`](conformance-matrix.json) 为准。

本轮 `--workspace` 实跑（2026-11-15，本机受限会话）：harness 用
`cargo test --offline --workspace --no-fail-fast` 跑完 **66 suites / 796 passed / 1 failed**
（根 workspace 已包含 `dshr-plugin-wasm`，无需 exclude）；逐包判定 implemented 441 pass /
1 fail（仅 `CARGO-UNIT-001` workspace-wide）/ 34 skip。唯一失败仍是 `dshr-sidecar-guard` 的
`process::tests::drop_terminates_the_guarded_process`（隔离重跑同样失败，属沙箱进程限制；
普通终端/Lead 环境同一 workspace 为 **66 suites / 797 passed / 0 failed**；[`m3-status.md`](m3-status.md)
§4 已记录）。提交矩阵用 `--quick` 生成、cargo ID 记为 SKIP。

主要 groups：

| 领域 | 状态 | 证据/前置 |
|---|---|---|
| `SCHEMA-FIXTURE-001..020` | implemented | `schema/check.mjs` fixture 逐条登记 |
| `DOC-LINK-001` | implemented | `tools/check-links.mjs` 0 broken |
| `JS-L2SVC-001..013` | implemented（`quick`） | `node-test` suite：`sidecar/dsh-official/lib/host-services.test.mjs`（源码 ID L2JS-001..013） |
| `CARGO-UNIT-001..004` | implemented（`--workspace` 才执行） | workspace 单测；按包 selector |
| `ADC-SLOT-001..023` / `ADC-ELEM-001..022` / `ADC-SEC-001..009` | implemented | `dshr-ui-contributions`（ADC-S） |
| `ADC-ELEM-SPACING/ALIGN/...-001..004`（26 条）+ `ADC-ELEM-PANEL-001..010` | implemented | `dshr-ui-gpui` 渲染器/panel 入口单测（ADC-R） |
| `ADC-ELEM-PANEL-API-001..002` | implemented | `dshr-plugin-api` panel 渲染契约默认实现 |
| `ADC-ELEM-PANEL-HOST-001..004` | implemented | `dshr-plugin-host` panel 渲染入口（含 external-process `ui.render`） |
| `ADC-PERF-001..002` | implemented（`--workspace` 才执行） | `dshr-ui-gpui` `tests/adc_perf.rs` 构造预算（debug 约 4.4s；release 见 `m3-render-budget.md`） |
| `CPP-FIBER-001..027` / `CPP-SVC-001..014` / `CPP-EFF-001..010` | implemented | `dshr-plugin-host`（Fiber/Service/Effect） |
| `CPP-FIBER-101..133` | implemented | `dshr-plugin-api`（capability/取消/结果/错误码） |
| `CPP-MANIFEST-001..026` / `CPP-VER-001..007` | implemented | `dshr-plugin-api`（typed/TOML manifest、版本） |
| `CPP-TRUST-001..018` | implemented | `dshr-profile`（摘要/签名/首装确认/降级） |
| `CPP-PROC-001..010,020..032` | implemented | `dshr-plugin-sdk`（external-process 协议/会话 SDK） |
| `CPP-ISO-001..015,020..021` | implemented | `dshr-plugin-host`（external-process 生命周期/隔离/安全/G2 E2E） |
| `CPP-WASM-001..020` | implemented（`--workspace`；首次编译 wasmtime 1–3 分钟） | `dshr-plugin-wasm`（wasmtime 主机：fuel/epoch/内存/输出/并发配额四类隔离 + core-module ABI；见 `wasm-host.md`） |
| `MCP-CLIENT-001..017` | implemented | `dshr-compat-mcp`（MCP stdio 客户端，源码 ID MCP-001..017） |
| `RPC-L2-001..010` | implemented | `dshr-compat-dsh-protocol`（v2 envelope/service_key） |
| `RPC-L2-101..128,221..227,301..313` | implemented | `dshr-compat-dsh-host`（service 代理 + storage_kv + 新服务方法） |
| `RPC-L2-401..410` | implemented | `dshr-compat-dsh-host` `tests/l2_host_services.rs`（v2 握手/caps、storage/wd 端到端、cancel/迟到/generation） |
| `RPC-L2-501..505` | implemented（官方包缺失时测试内部 SKIP） | `dshr-compat-dsh-host` `tests/official_packages.rs`（未修改官方包装载/storage-domain 往返；依赖 `.tmp/dsh-tools-all` + vendored sidecar） |
| `M3-LOADER-001..015` | implemented | `dshr-profile` `loader.rs`（include/group/patch/深度/`!!js`） |
| `M3-DEFAULT-001..004,010..015,020..025,030..032,035..037,040..045,050..056,060..063` | implemented | agent-core / llm-retry / llm-deepseek / llm-openai / agent-context / tool-runtime（DSH 默认对齐） |
| `M3-PARALLEL-001..007` | implemented | `dshr-agent-core` batch 调度：并发/顺序/预算截断/错误隔离/max_parallel 解析/重复提醒/取消 |
| `M3-PARALLEL-101..107` | implemented | `dshr-compat-dsh-host` `tests/parallel_batch.rs`（sidecar 批 invoke 端到端） |
| `ADC-BIND-001` | pending | 绑定权限尚未实现（WASM 配额/隔离已由 CPP-WASM-001..020 覆盖） |
| `SELF-CHAT-001..017` | pending | 现有 17 个 `chat --selftest*`，尚未纳入 harness |
| `CPP-TRUST-019` / `CPP-ISO-022` / `RPC-L2-411` / `ADC-PANEL-001` / `M3-DEFAULT-070` / `M3-PARALLEL-201` / `CPP-AUTHOR-001` / `GATE-G2-001` | manual | chat selftest（信任链/外部插件/L2/ADC panel/defaults/工具并行）、authoring 门禁、外部作者实验需人工判定 |

## 7. 与 M3 exit gate 的对应

参考 [`review-m3-plan.md`](review-m3-plan.md) §4：

- **G1 契约冻结（ADC）**：`ADC-SLOT-*`（12 slot 目录 + 注册/撤销/委托）、`ADC-ELEM-*`
  （ui-contributions 元素模型 + ui-gpui 26 元素 + 10 个 panel 入口 + panel API/HOST 契约，
  未知 type → Placeholder）、`ADC-SEC-*`（https-only/配额/a11y）、`ADC-PERF-*`（渲染构造预算）
  已 implemented；manual 证据 `ADC-PANEL-001`（chat `--selftest-adc-panel`：装载→panels→
  render_panel→未知元素 view）；`ADC-BIND-*`（绑定权限/patch seq）仍 pending。
- **G2 外部作者可发布**：`CPP-TRUST-001..018`（签名/首装确认/降级）、`CPP-ISO-001..021`
  （external-process 生命周期/隔离 + G2 安装→装载→调用→卸载 E2E）、`CPP-PROC-*`（SDK）、
  `CPP-MANIFEST/VER`、`M3-LOADER-001..015`（include/group/patch/`!!js` 语义）、
  `M3-DEFAULT-*`（DSH 默认差异对齐）已 implemented；manual 证据：`CPP-TRUST-019`
  （chat `--selftest-trust-install`）、`CPP-ISO-022`（chat `--selftest-external-plugin`）、
  `M3-DEFAULT-070`（chat `--selftest-defaults`）、`CPP-AUTHOR-001`
  （`tools/authoring-check.ps1`）、`GATE-G2-001`（仓库外实验 checklist）。
- **G3 隔离与兼容（WASM 为主方案）**：`CPP-WASM-001..020`（wasmtime 主机：
  fuel/epoch/内存/输出/并发配额四类隔离 + core-module ABI，见 [`wasm-host.md`](wasm-host.md)）、
  `RPC-L2-*`（protocol v2 契约 + host service 代理 + storage_kv + 新服务方法 +
  `RPC-L2-401..410` 真实 sidecar 端到端 + `RPC-L2-501..505` 未修改官方包装载/往返）、
  `MCP-CLIENT-*`（MCP stdio 客户端）、`JS-L2SVC-*`（sidecar host-service JS 单测）、
  `M3-PARALLEL-001..007`（agent-core batch 调度）+ `M3-PARALLEL-101..107`（sidecar 批 invoke
  端到端）已 implemented；manual 证据 `RPC-L2-411`（chat `--selftest-l2`：phase A 真实 sidecar
  protocol=2 + phase B fixture storage-kv）、`M3-PARALLEL-201`（chat `--selftest-parallel-tools`：
  串行 6.1s→并行 2.0s，ratio 0.33，顺序约束成立）。

gate 判定方式：`node tools/conformance.mjs --full` 全绿（implemented 无 FAIL）且对应 gate 的
manual 证据落盘。

## 8. 已知缺口

1. **17 个 chat selftest 未纳入**：`SELF-CHAT-001..017` 已登记为 pending。未来入口：新增
   `selftest-chat` suite（harness 内置 runner + 注册表 `suites` 同步），执行
   `tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest*`，按输出
   `[selftest] PASS` 判定。注意其中 `--selftest-dsh*` / `--selftest-cordis` 依赖本机 DSH 安装副本，
   CI 环境不可用，需要标 manual 或加能力探测后 SKIP。
2. **cargo 不在 quick**：避免每次门禁都构建 Rust；`suite=cargo` 的 implemented ID 只有
   `--workspace`/`--full` 或 CI 的独立 `cargo test` 步骤才执行。quick 矩阵里它们是 SKIP，不是失败。
   第三方 JS 单测走 `node-test`（quick），不需要 Rust。
3. **schema selector 与用例 label 耦合**：`SCHEMA-FIXTURE-*` 的 selector 指向
   `schema/check.mjs` 的 `[schema] label`。重命名/删除用例会让对应 ID FAIL——这是有意为之
   （注册表引用的证据必须仍然存在）。新增用例同时把新 ID 写进注册表，不要只改脚本。
4. **ADC/CPP/RPC/M3 已有运行期证据，但不是全绿**：ADC-SLOT/ELEM/SEC/PANEL/PERF（含 panel API/HOST）、
   CPP-FIBER/SVC/EFF/MANIFEST/VER/TRUST/PROC/ISO/WASM、MCP、RPC-L2（含 4xx/5xx 端到端）、
   M3-LOADER/M3-DEFAULT/M3-PARALLEL 已按 crate 单测登记为 implemented（`--workspace`/CI 才执行）；
   剩余 `ADC-BIND-001`（绑定权限）仍 pending，不能因为矩阵整体 exit 0 就宣称对应 gate 已完成
   （quick 不含 cargo）。
5. **WASM 测试首次编译慢**：`dshr-plugin-wasm` 依赖 wasmtime，首次 `--workspace` 需 1–3 分钟
   编译；`--timeout-ms` 要给足（默认 20 分钟足够）。`CPP-WASM-*` 的 selector 是
   `dshr-plugin-wasm`，不是整个 workspace。
6. **`groups` 依赖源码扫描**：批量条目靠 `source_dirs` + 测试函数名/注释匹配（支持 `// ID：` 与
   `/// ID：` 两种注释；W7 的 M3-PARALLEL-007 是 `///` 文档注释 + 非 ID 函数名）。改测试函数名或
   删测试会让对应 ID 注册表校验失败——这是有意的证据校验；新增测试时把 ID 加进对应组的 `ids`。
7. **两段式 ID 的映射**：`compat-mcp` 源码写 `MCP-001..017`、sidecar JS 测试写 `L2JS-001..013`，
   都是两段式；注册表按 spec §3 的三段式登记为 `MCP-CLIENT-001..017` / `JS-L2SVC-001..013`，
   title/notes 保留源码 ID。若日后 spec 放宽 ID 形态，可再收敛。
8. **RPC-L2-501..505 可能内部 SKIP**：这些测试依赖 `.tmp/dsh-tools-all`（gitignored 的官方包抽取）
   与 vendored sidecar；缺失时测试打印 SKIP 但仍算 `cargo test` 通过。矩阵显示 PASS 不等于
   "官方包已在本机实跑"，需看该 crate 的测试输出；`docs/m3-gate-evidence.md` 记录实际取数环境。
9. **CI 未实跑**：`.github/workflows/conformance.yml` 是本仓库首个 workflow，尚未在
   GitHub Actions 验证；矩阵报告里明确标注「未在 GitHub Actions 实跑」。本地等价命令即
   `schema/check.mjs` + `check-links.mjs` + `node --test ...` + `cargo test --offline --workspace` +
   `--quick` 矩阵。
10. **blocked 暂无条目**：状态机保留该状态；首次出现真实阻塞（如 WASM 配额 ADR 未决）时再登记。

## 9. 维护流程（加一个测试 ID）

1. 先有证据：脚本用例 / Rust 单测 / 人工步骤，再登记 ID；**不要先写 ID 再补证据**。
2. 单个测试改 [`conformance.d/registry.json`](conformance.d/registry.json) 的 `tests` 条目
   （`id`/`area`/`title`/`status`/`suite`/`selector`/`notes`，无证据的 suite/selector 写 `null`）。
3. 成批的 crate 单测加/改 `groups` 条目：给 `source_dirs` + `ids`（可选 `--registry` 指向
   candidate 先验证），harness 会扫描源码确认每个 ID 有测试函数；`selector` 用该 crate 的
   cargo 包名。新增测试时把新 ID 追加进组 `ids`，不要只改源码。
4. `implemented` 的 selector 必须能被对应 suite 输出命中；拿不准先跑 `--suite <name>` 看输出。
5. 跑 `node tools/conformance.mjs --quick`，再按需重生成矩阵：
   `--quick --compact --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md`。
   460+ 条 ID 建议始终带 `--compact`；quick 现在包含 schema/links/node-test。
6. 矩阵文件是生成的，**不要手改**；CI 会重新生成并上传 artifact。
7. 新增 suite 属于 harness 变更（`SUITE_NAMES`/`SUITE_COMMANDS`/runner/解析器），需同时改
   [`tools/conformance.mjs`](../tools/conformance.mjs) 与注册表 `suites`，两处不一致时 harness 直接报错。
   `node-test` 是第三轮新增的 quick suite：JS 测试按名称匹配 selector，新增 JS 用例时同步加
   注册表条目（类比 `JS-L2SVC-*`）。
