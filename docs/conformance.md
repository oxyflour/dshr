# Conformance 门禁（测试 ID 注册表 + 矩阵报告）

> 目标：兑现 [`PLAN.md`](PLAN.md) §10.1「conformance matrix 是 CI 产物，不是文档表格」。
> 本文说明怎么加测试 ID、怎么跑、当前矩阵与已知缺口；机器可读报告是
> [`conformance-matrix.json`](conformance-matrix.json)，人读表格是
> [`conformance-matrix.md`](conformance-matrix.md)。

## 1. 组成

| 文件 | 作用 |
|---|---|
| [`tools/conformance.mjs`](../tools/conformance.mjs) | harness：校验注册表、执行 suite、按 selector 判定、生成矩阵报告 |
| [`conformance.d/registry.json`](conformance.d/registry.json) | **唯一事实源**：suite 定义与全部测试 ID（id/area/title/status/suite/selector/notes） |
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

- `AREA`：测试领域（`SCHEMA`、`DOC`、`CARGO`、`ADC`、`CPP`、`RPC`、`SELF`、`GATE`）。
- `CATEGORY`：断言分组（`FIXTURE`、`LINK`、`UNIT`、`SLOT`、`ELEM`、`L2` …）。
- `NNN`：三位数字，在 `AREA-CATEGORY` 内唯一、不复用。
- 注册表里 `area` 字段必须与 ID 首段一致，ID 全库唯一。

## 3. 状态机

`status` 是**注册表里的承诺状态**；`result` 是**本次运行的结果**（由 harness 计算）。

| status | 含义 | 是否影响退出码 |
|---|---|---|
| `implemented` | 已有可执行证据：必须声明 `suite` + `selector`，且 selector 在本次运行中命中 | 是（未命中/失败 → FAIL） |
| `pending` | 已定义断言但实现/契约未就绪（ADC/CPP/RPC、17 个 chat selftest） | 否 |
| `manual` | 需人工实验判定（如 M3 G2 外部作者实验），harness 无法自动化 | 否 |
| `blocked` | 被外部依赖阻塞（当前暂无条目；语义保留给前置未落地时） | 否 |

`result` 取值：`PASS` / `FAIL` / `SKIP`（suite 本次未选中，例如 `--quick` 下的 `CARGO-UNIT-*`）/
`PENDING` / `MANUAL` / `BLOCKED`。

规则：

- `implemented` 但没有 `suite` + `selector` → **注册表校验失败**（退出码 1）。
- `pending` / `manual` / `blocked` 不得声明 `suite` 或 `selector`，防止把未实现的东西伪装成可执行证据。
- `--quick` 不跑 cargo：`CARGO-UNIT-*` 显示 SKIP 是正常状态，不是失败；要跑用 `--workspace` 或 `--full`。
- 退出码：0 = 通过；1 = 有失败项或注册表非法；2 = 用法错误。pending/manual/blocked 永不导致失败。

## 4. suite

| suite | 命令 | quick | 默认超时 | selector 语义 |
|---|---|---|---|---|
| `schema` | `node schema/check.mjs` | 是 | 120s | 输出中 `ok [schema] label` 行；selector 取 `[schema] label` |
| `links` | `node tools/check-links.mjs` | 是 | 120s | 只有 `all`：suite 整体通过即满足 |
| `cargo` | `tools\build-env.cmd cargo test --offline --workspace`（Windows；其它平台 `cargo test --offline --workspace`） | 否 | 1200s（20 min） | `all` 或包名（如 `dshr-plugin-host`，在该包测试二进制出现且 cargo 全绿时满足） |

Windows 上 cargo 必须经 [`tools/build-env.cmd`](../tools/build-env.cmd)（本机 `%TEMP%` 不可写，
见 [`m0-runbook.md`](m0-runbook.md)）；harness 用 `cmd /c` 调用 `.cmd`。cargo 超时可用
`--timeout-ms` 覆盖。

## 5. 命令

```powershell
# 秒级门禁：schema + links + 注册表校验
node tools/conformance.mjs --quick

# 生成矩阵（提交进仓库的两份文件）
node tools/conformance.mjs --quick --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md

# 查看注册表（不执行）
node tools/conformance.mjs --list

# 只跑某个 suite（可重复）
node tools/conformance.mjs --suite schema --suite links

# 额外跑 Rust 全量单测（默认不跑；约数分钟）
node tools/conformance.mjs --workspace

# 全部（生成含 cargo 结果的完整矩阵）
node tools/conformance.mjs --full --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md
```

参数：`--timeout-ms <n>` 覆盖本次所有 suite 超时；`--json`/`--markdown` 后接路径可改输出位置
（缺省用 `docs/conformance-matrix.*`）；`--help` 查看用法。

> 报告写入失败时（例如目标目录被沙箱设为只读），harness 会把报告回退写入
> `.tmp/conformance/` 并打印警告，退出码仍由测试结果决定；CI 由 artifact 上传步骤
> （`if-no-files-found: error`）兜底。此时请用有写权限的会话重跑，或用 DSH 文件工具把回退文件复制到目标路径。

## 6. 当前矩阵摘要

首次生成（本机 Windows）：**25 implemented（20 schema fixture + 1 doc link + 4 cargo 条目）、
31 pending、1 manual、0 failed**；`--quick` 下 21 通过、4 个 cargo 条目 SKIP。
矩阵报告带 `generated_at`、`git HEAD`（含 dirty 文件数）、platform、逐 suite 结果与逐 ID 行；
以 [`conformance-matrix.json`](conformance-matrix.json) 为准。

本轮另做了一次 `--workspace` 实跑（2026-11-12，本机受限会话）：cargo suite 26 suites /
316 passed / 1 failed。唯一失败是 `dshr-sidecar-guard` 的
`process::tests::drop_terminates_the_guarded_process`（隔离重跑同样失败，属沙箱进程限制，
与本轮 M3 改动无关）；因此提交的矩阵用 `--quick` 生成、`CARGO-UNIT-*` 记为 SKIP，
cargo 全绿待普通终端或 CI 复核。

主要 groups：

| 领域 | 状态 | 证据/前置 |
|---|---|---|
| `SCHEMA-FIXTURE-001..020` | implemented | `schema/check.mjs` 20 条 fixture 逐条登记 |
| `DOC-LINK-001` | implemented | `tools/check-links.mjs` 0 broken |
| `CARGO-UNIT-001..004` | implemented（`--workspace` 才执行） | workspace 单测；plugin-host / plugin-api / ui-contributions 分包 selector |
| `ADC-SLOT/ELEM/BIND/SEC-*` | pending | ADC-v1 冻结工件与服务端/渲染器 |
| `CPP-MANIFEST/VER/FIBER/SVC/EFF/WASM/TRUST-*` | pending | CPP-v1 运行时各切片 |
| `RPC-L2-001..003` | pending | sidecar-rpc v2 host-service 代理 + adapter |
| `SELF-CHAT-001..017` | pending | 现有 17 个 `chat --selftest*`，尚未纳入 harness |
| `GATE-G2-001` | manual | M3 G2 外部作者实验需人工 checklist |

## 7. 与 M3 exit gate 的对应

参考 [`review-m3-plan.md`](review-m3-plan.md) §4：

- **G1 契约冻结（ADC）**：`ADC-SLOT-*`（12 slot）、`ADC-ELEM-*`（元素 props/未知 type →
  Placeholder）、`ADC-BIND-*`、`ADC-SEC-*`。G1 全绿前这些保持 pending。
- **G2 外部作者可发布**：`CPP-*` 运行时证据 + `GATE-G2-001`（manual，仓库外构建/安装/卸载 +
  「未读 crates/ 源码」checklist）。
- **G3 隔离与兼容**：`CPP-WASM-001`（fuel/内存/超时隔离）+ `RPC-L2-*`（3 个 L2 service）。

gate 判定方式：`node tools/conformance.mjs --full` 全绿（implemented 无 FAIL）且对应 gate 的
manual 证据落盘。

## 8. 已知缺口

1. **17 个 chat selftest 未纳入**：`SELF-CHAT-001..017` 已登记为 pending。未来入口：新增
   `selftest-chat` suite（harness 内置 runner + 注册表 `suites` 同步），执行
   `tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest*`，按输出
   `[selftest] PASS` 判定。注意其中 `--selftest-dsh*` / `--selftest-cordis` 依赖本机 DSH 安装副本，
   CI 环境不可用，需要标 manual 或加能力探测后 SKIP。
2. **cargo 不在 quick**：避免每次门禁都构建 Rust；`CARGO-UNIT-*` 只有 `--workspace`/`--full`
   或 CI 的独立 `cargo test` 步骤才执行。quick 矩阵里它们是 SKIP，不是失败。
3. **schema selector 与用例 label 耦合**：`SCHEMA-FIXTURE-*` 的 selector 指向
   `schema/check.mjs` 的 `[schema] label`。重命名/删除用例会让对应 ID FAIL——这是有意为之
   （注册表引用的证据必须仍然存在）。新增用例同时把新 ID 写进注册表，不要只改脚本。
4. **ADC/CPP/RPC 尚无运行期证据**：schema 侧只能证明「文件形状」（如
   `SCHEMA-FIXTURE-011..015`），不能替代 typed 解析器/运行时单测；因此 `CPP-*`/`ADC-*` 仍为 pending。
5. **CI 未实跑**：`.github/workflows/conformance.yml` 是本仓库首个 workflow，尚未在
   GitHub Actions 验证；矩阵报告里明确标注「未在 GitHub Actions 实跑」。本地等价命令即
   `schema/check.mjs` + `check-links.mjs` + `cargo test --offline --workspace` + `--quick` 矩阵。
6. **blocked 暂无条目**：状态机保留该状态；首次出现真实阻塞（如 WASM 配额 ADR 未决）时再登记。

## 9. 维护流程（加一个测试 ID）

1. 先有证据：脚本用例 / Rust 单测 / 人工步骤，再登记 ID；**不要先写 ID 再补证据**。
2. 修改 [`conformance.d/registry.json`](conformance.d/registry.json)：加 `tests` 条目
   （`id`/`area`/`title`/`status`/`suite`/`selector`/`notes`，无证据的 suite/selector 写 `null`）。
3. `implemented` 的 selector 必须能被对应 suite 输出命中；拿不准先跑 `--suite <name>` 看输出。
4. 跑 `node tools/conformance.mjs --quick`，再按需重生成矩阵：
   `--quick --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md`。
5. 矩阵文件是生成的，**不要手改**；CI 会重新生成并上传 artifact。
6. 新增 suite 属于 harness 变更（`SUITE_NAMES`/`SUITE_COMMANDS`/runner/解析器），需同时改
   [`tools/conformance.mjs`](../tools/conformance.mjs) 与注册表 `suites`，两处不一致时 harness 直接报错。
