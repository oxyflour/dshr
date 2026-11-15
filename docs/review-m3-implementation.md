# M3「插件平台 Preview」实现独立 review（2026-11-15）

reviewer: review-m3-implementation（fresh context，只读评审；本文件是唯一写入产物）。

范围：评审开始时 HEAD `e10ba8f`（master，工作区干净）；评审过程中上游又推进了两个**文档/注册表**
提交 `5dee164`（M3-PARALLEL 并入 registry/矩阵）与 `2baa48b`（README 收尾），代码未变
（除 `tools/conformance.mjs:212` 扫描正则 2 行），因此本文的代码结论按 `e10ba8f`、conformance
数字按当前 HEAD 记录。依据 `docs/PLAN.md` §8 M3、[`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)
§4、[`0010`](decisions/0010-isolation-runtime-quota.md)、[`0011`](decisions/0011-adc-v1-freeze.md)，
并交叉核对 `m3-status.md`、`m3-gate-evidence.md`、spec/schema/registry 与实际代码。**不照抄状态文档**：
下面每个数字都是本轮自己跑的，证据不足或依赖被跳过测试的地方单独标出。

## 结论（verdict）

**不接受（not accepted）——exit gate 没有全部成立；G3 的核心 gate 项不成立，G2 只是有条件通过。**

- **G1（ADC-v1 冻结）通过。** spec `Frozen`、12 slot、71/71 schema、26 元素字段级、未知元素
  `Placeholder` + 诊断 + 不 panic 都有真实测试；`--selftest-adc-panel` 也真跑通（panels=3 /
  rendered=3 / legal_diags=0 / unknown_diag=true）。限制是"产品面只构造到 `AnyElement`"和
  "tool card/交互/焦点在 M4"（后者已被 ADR-0011 明示降级，不算 gate 失败）。
- **G2（外部作者可发布）有条件通过，按字面口径不通过。** 安装→装载→调用→卸载、首装确认、
  审计、篡改/降级拒绝都实测通过；但"只用 semver SDK 在仓库外构建"并不成立：模板/生成器给的是
  `dshr-plugin-sdk = { path = "<repo>/crates/plugin-sdk" }`，SDK 未发布到任何 registry，生成样例
  仍在 `.tmp/`。没有这个 repo checkout，第三方作者无法构建。另有安装非原子（先删旧目录再复制）
  与无 `--uninstall-plugin` 两项生命周期缺口。
- **G3（隔离与兼容）不通过。** WASM 只做到 `crates/plugin-wasm` 的独立引擎：`plugin-host` 没有
  wasm runtime 分支，chat 的 profile 装载器对 `wasm-component` 直接 `continue`（静默忽略），
  状态文档却写"已集成"。WASM 内存配额只限制了**每条 linear memory**，没有限制 memory 条数、
  table 元素数、instance 数（wasmtime 默认 table 不限、memory 条数 10000），可绕过
  `max_memory_mb`。三个 L2 service 的"未修改官方插件跑通"实际只覆盖 ~1.5 个：
  `dsh-tool-working-directory` 装载绿但 `execute` 被 `exec.agent` 阻塞；
  官方 `dsh-storage-domain` 经 storage-kv 真往返；`sessionProjections` adapter 与
  `RPC-L2-201..208` 根本不存在，session-log 没有官方插件消费者。
- **P0 一条：G3 官方包验收可被 SKIP 掩盖。** 缺 `.tmp/dsh-tools-all` 时
  `official_packages.rs` 直接 `return`，cargo 仍报 "5 passed"；conformance 的 cargo 判定是
  **按包**而不是按测试，所以 `RPC-L2-501..505` 仍显示 pass。CI 不 vendor 这些包，因此默认就是
  "没有官方包也全绿"。这是**gate 虚假**，不是普通测试缺口。

> 若产品/架构愿意把 G3 正式改口径为"ADR-0010 的 external-process 退路 + WASM core-module 引擎级
> 隔离（不含宿主接入）"，并把 G2 改为"内部作者 Preview"，则可降为**有条件接受**；条件即下文
> P0/P1 全部关闭。**但当前 PLAN/ADR-0007 的 gate 文本没有做这个变更**，所以按现有 gate 判不接受。

## 本轮验证清单（命令 + 实测结果）

| # | 命令 | 实测结果 |
|---|---|---|
| 1 | `node schema/check.mjs` | **71/71 cases passed**，exit 0 |
| 2 | `node tools/check-links.mjs` | **318 links / 0 broken / 61 files**，exit 0 |
| 3 | `node tools/conformance.mjs --quick` | registry 502 ID；**implemented 34/476 通过，失败 0，跳过 442**，exit 0 |
| 4 | `node tools/conformance.mjs --workspace` | **implemented 442/476 通过，失败 0，跳过 34**（34 个是 quick suite），exit 0；cargo 66 suites / 797 passed / 0 failed |
| 5 | `node tools/conformance.mjs --full` | **implemented 476/476 通过，失败 0，跳过 0**，exit 0（本机有 `.tmp` + vendored sidecar） |
| 6 | `tools\build-env.cmd cargo test --offline --workspace --no-fail-fast` | **66 suites / 797 passed / 0 failed / 0 ignored**；首跑经 `Tee-Object` 管道返回 exit 1 但日志 66/797/0，复跑直接执行 exit 0——判为 harness/管道噪声，不是测试失败 |
| 7 | `tools\build-env.cmd cargo test --offline -p dshr-plugin-wasm` | **20 passed / 0 failed** |
| 8 | `node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs` | **13/13 passed** |
| 9 | `target\debug\chat.exe --selftest-trust-install` | **PASS**：`rejected_unconfirmed=true installed_ok=true tamper_rejected=true audit_accepted=true audit_rejected=true` |
| 10 | `--selftest-external-plugin` | **PASS**：`installed_ok=true echo_ok=true unloaded=true reload_ok=true panel_ok=true` |
| 11 | `--selftest-adc-panel` | **PASS**：`panels=3 rendered=3 panel_errors=0 legal_diags=0 unknown_diag=true` |
| 12 | `--selftest-l2`（`TEMP=.tmp`，即 build-env 语义） | **PASS**：phase A `protocol=2 active=true host_services=3`；phase B `roundtrip=ok` |
| 13 | `--selftest-l2`（直接用用户 `%TEMP%`） | **FAIL**：phase B `storage-kv: create storage root: 拒绝访问 (os error 5)`；同一台机、同一二进制，只改 TEMP 即 PASS——环境项，但 selftest 不健壮（见 P2-4） |
| 14 | `--selftest-defaults` | **PASS**：approval/repeat/timeout/retain/reasoning 全 true |
| 15 | `--selftest-parallel-tools` | **PASS**：`serial=6.135s parallel=2.046s ratio=0.33 order_ok=true` |
| 16 | `--selftest-dsh` | **PASS**：`read_tool_ok=true replay_equal=true` |
| 17 | `powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1` | **PASS**：生成→离线构建→打包→安装 0.1.0→升级 0.2.0→降级 `E_TRUST_DOWNGRADE`→篡改 `E_INTEGRITY_MISMATCH`→未确认 exit 2 |
| 18 | 官方包 SKIP 复现（见 P0） | 临时重命名 `.tmp/dsh-tools-all/dsh-tool-working-directory` 后跑 `cargo test -p dshr-compat-dsh-host --test official_packages`：**5 passed / 1.00s**（正常有包时 9.90s），exit 0；随后已恢复目录 |

未跑：release 版 render budget 复测、真实 GUI 窗口/帧探针、GitHub Actions、真实网络 LLM。
这些在"未验证项"里单列。

## Gate 逐条对账

### G1 — ADC-v1 契约冻结：**通过**

| 判定口径（ADR-0007 §4） | 实际证据 | 判定 |
|---|---|---|
| spec 状态 `Frozen` | `docs/spec/adc-v1.md:3` 明确 `Frozen`；ADR-0011 Accepted | 通过 |
| 12 slot 与 spec §5 一一对应 | `schema/adc-v1/slots.v1.json` 12 条；schema/check 71/71 含目录自检 | 通过 |
| 元素 props 字段级 | `element.schema.json` 26 元素；正反例 23 条（含 A11Y-001） | 通过 |
| §12 未决清零 | `adc-v1.md` §12 Q1–Q5 全部有结论（Q5 明确排除 WebView） | 通过 |
| 未知元素 `Placeholder` + 诊断 + 不 panic，同树其余仍渲染 | `ui-contributions` 22 个 ADC-ELEM 测试含 `adc_elem_002_unknown_element_degrades_to_placeholder_without_losing_tree`；`adc_sec_007_malformed_trees_never_panic`；`ui-gpui` 79 tests 含 26 元素 + `panel_unknown_root_falls_back_to_placeholder` | 通过 |
| panel 产品入口 | `adc_panel_body` + `PluginHost::render_panel` + chat 右栏；`--selftest-adc-panel` PASS | 通过（仅构造层） |

**G1 的真实限制（不构成 gate 失败，但必须写清）：**

1. `--selftest-adc-panel` 是"无窗口"自检：`adc_panel_body` 返回的 `AnyElement` 立刻 `drop`
   （`apps/desktop-gpui/src/bin/chat.rs:6295-6316`、`:6342-6345`），没有窗口/布局/paint 验证；
   `adc_perf.rs` 测的是 parse+view 预算，不是 draw。ADR-0011 已写明"渲染预算窗口内 draw 留 M4"，
   所以这不是冻结点造假，但"产品面板已接"只能理解为"元素构造链路已接"。
2. 产品路径只消费 `workspace.panel` 一个 slot（`chat.rs:1912` 过滤），其余 11 个 slot 的服务端
   注册表没有进入 chat 的 panel 路径；`PluginHost` 的 panel 是独立 `BTreeMap`，不经过
   `ui-contributions::ContributionRegistry`。因此"12 slot 服务端"是库层事实，不是产品面事实。
3. `ADC-BIND-001`（绑定/事件/焦点）仍 pending；tool card 的 ADC renderer 未接——ADR-0011 降级清单
   已明示，M4 项。

### G2 — 外部作者可发布：**有条件通过（按字面口径不通过）**

| 判定口径（ADR-0007 §4） | 实际证据 | 判定 |
|---|---|---|
| 只用文档 + 模板 + **semver SDK** + schema，在仓库外构建 Tool+Panel | 文档/模板/生成器/打包器齐全；authoring-check 生成在 `.tmp/authoring` 并构建成功；**但模板依赖是仓库绝对路径** `dshr-plugin-sdk = { path = "__DSHR_REPO__/crates/plugin-sdk" }`（`templates/external-tool-plugin/Cargo.toml:20`），SDK 未发布，第三方没有 repo 无法构建 | **不满足字面** |
| 安装 → 装载 → 调用 → 卸载 | `--selftest-external-plugin` PASS（安装/装载/echo/卸载/重载/panel 全 true）；安装走 `install_package_trusted` | 通过（运行期 unload） |
| 首装确认 / 审计 / 来源校验 | `--selftest-trust-install` PASS；`trust.rs` 18+ tests；审计 accepted/rejected 都写 | 通过 |
| 篡改包与旧版本降级被拒 | authoring-check 实测 `E_INTEGRITY_MISMATCH`、`E_TRUST_DOWNGRADE`；trust 单测覆盖 | 通过 |
| 签名（可选） | ed25519 校验实现 + 单测；**无发布侧签名工具**（M3 不要求），CLI 仍只测 unsigned | 部分（可接受） |
| 卸载生命周期 | **无 `--uninstall-plugin` CLI**，文档要求手动删 `node_modules/<name>` 与 profile 条目（`docs/plugin-authoring.md:329`） | 不满足完整生命周期 |

**为什么判"有条件"而不是"通过"：** gate 文本的核心是"第三方不读主程序源码也能构建"。当前
`plugin-authoring.md:96,267` 自己承认"模板以 path 依赖引入；对外发布后改 semver"——这是
**计划中的未来状态**，不是 M3 已交付的能力。authoring-check 的"仓库外"只是"repo 目录外的 `.tmp`"，
`Cargo.toml` 仍指回 `C:/Projects/dshr/crates/plugin-sdk`。如果产品接受"内部作者 Preview + 等 SDK
发布"，可以作为 G2 的降级口径，但必须写进 ADR/状态，而不是标成已通过。

### G3 — 隔离与兼容：**不通过**

#### 3.1 WASM 隔离（gate 的"主方案"部分）

| 判定口径 | 实际证据 | 判定 |
|---|---|---|
| fuel 耗尽隔离、宿主存活 | `CPP-WASM-001..020` 真测试：`OutOfFuel`、同 runtime 继续可用 | 通过（引擎级） |
| epoch 超时隔离 | 100ms epoch → `E_TIMEOUT`；trap 后同 runtime/relaunch | 通过（引擎级） |
| 线性内存超限 | 单 memory `StoreLimits` → `E_QUOTA(MemoryLimit)` | 部分：只限单条 memory |
| 输出超限 | `out_len > max_output_bytes` 先检查再读；1MiB 结果 > 4KiB → `E_TOO_LARGE` | 通过（单结果） |
| 并发上限 | 每 runtime 4 → `E_BACKPRESSURE`；**没有全局 16** | 部分 |
| **接入宿主/产品路径** | `plugin-host` 无 wasm 分支；`chat.rs:6055` 对 `runtime.kind != ExternalProcess` 直接 `continue`；`wasm-host.md:48` 自述"尚未接线" | **不通过** |
| **Session 无脏写** | 没有任何 wasm/session 交互测试（因为 wasm 根本不能装载） | **无证据** |

**关键代码证据：**

- `crates/plugin-wasm/src/lib.rs:40-47`：注释"接入 `plugin-host`（本轮不接）"，建议接入点而非实现。
- `crates/plugin-host/src/lib.rs:116-131`：`load_with_manifest` 只做 manifest 校验 + `Box<dyn Plugin>`，
  没有 `RuntimeKind` 分发；`crates/plugin-host/Cargo.toml` 也不依赖 `dshr-plugin-wasm`。
- `apps/desktop-gpui/src/bin/chat.rs:6055-6057`：`if manifest.runtime.kind != RuntimeKind::ExternalProcess { continue; }`
  ——安装一个 `wasm-component` 插件后，启动时**静默不加载、不报错**，用户只会看到"没效果"。
- `docs/wasm-host.md:48`：`plugin-host::load_with_manifest` 的 wasm 分支"尚未接线"；
  而 `docs/m3-status.md:99` 写"WASM 主机（W6，已集成）"、"已并回根 workspace"。
  **"并回 workspace" ≠ "接入宿主"**，这是状态文档的实质性夸大。

**内存配额的绕过（安全/隔离缺陷）：** `crates/plugin-wasm/src/lib.rs:508-519` 只构造
`StoreLimitsBuilder::new().memory_size(max_memory_bytes).trap_on_grow_failure(true)`。
wasmtime 50.0.1 的 `StoreLimits` 语义（本地 crate 源码 `src/runtime/limits.rs:230-283`）是：

- `memory_size` **按每条 linear memory 分别限制**；一个模块有 N 条 memory 时，每条都可到上限，
  而 `memories` 默认 10,000；
- `table_elements` 默认 **无限制**；`tables`/`instances` 默认 10,000；
- 当前代码没有设置 `table_elements`/`tables`/`memories`/`instances`。

因此一个恶意 core module 可以声明多条 memory 或一张巨大 table，绕过 `max_memory_mb`（例如
`(table 100_000_000 funcref)` 或 100 条 128MB memory），在实例化阶段消耗宿主内存/地址空间。
`CPP-WASM-MEM` 测试只覆盖单 memory 的 `memory.grow`，不能证明这条路径。这是 G3 "内存超限隔离"
的实质缺口，不是理论洁癖。

**其他 WASM 小问题：**

- `dshr_alloc` 返回 0 被视为合法指针（`lib.rs:456-463` 只判 `< 0`），而 `wasm-host.md:26` 写
  "0/负值视为失败"——ABI 文档与实现不一致，可能让 guest 在 0 地址写请求（仍在 guest 沙箱内，
  但结果错乱）。
- `WasmEngineConfig::from_manifest` 不接受输出上限/全局并发上限（只有 fuel/memory/timeout），
  ADR-0010 的"输出 1MB 默认 / 8MB 上限、全局 16"没有配置面。
- 每次 invoke 新建 Store 的代价/状态丢失已文档化（可接受），但没有池化/预编译；M4 项。

#### 3.2 external-process 退路（ADR-0010 的替代口径）

`CPP-ISO-001..017` 在本地全绿：超时 300ms→`E_TIMEOUT` 且 pid 死亡、1.2MiB 输出→`E_TOO_LARGE`、
512MiB@64MB→Job 杀、exit 3→宿主存活、卸载/重载无残留。**但仍有三个设计缺口：**

1. **输出配额可被"读之前"绕过（P1）**：`external.rs:777-796` 的 stdout reader 用
   `BufRead::read_line` 读取**完整一行**到 `String`，之后才在 `next_envelope`（`:497-498`）
   检查 `MAX_FRAME_BYTES`。恶意插件输出一条 1GB 的"行"会在检查前把宿主内存打爆；stderr
   reader（`:799-813`）同样无界。stdout 还通过 **无界** `mpsc::channel` 传递，快速刷行也会积压。
2. **写 stdin 的调用没有超时（P1）**：`exchange` 在 `:542` 才计算 deadline，但 `:541` 的
   `self.send(&request)?` 已经 `write_all` 到子进程 stdin；如果子进程不读管道，`write_all` 会
   阻塞，看门狗根本不会启动。一个不读 stdin 的插件可以永久卡住宿主的工作线程（G3 "宿主存活"
   在"进程活着但线程卡死"的意义上不成立）。
3. **Job Object 的 spawn→assign 竞态仍在（P2）**：`sidecar-guard/src/process.rs:237-246` 先
   `command.spawn()`，再 `AssignProcessToJobObject`。子进程在这个窗口派生的后代不在 job 内，
   `KILL_ON_JOB_CLOSE` 不保证"杀干净"。M2 review 的 P2-7 已指出，M3 未修。

#### 3.3 三个 L2 service 用未修改官方插件跑通

| service | 官方包证据 | 判定 |
|---|---|---|
| `working-directory/v1` | `dsh-tool-working-directory` 未修改装载绿（`RPC-L2-501`），`execute` 因 `exec.agent` 返回结构化错误（502）；`workingDirectory` 的 get/ensure/set 经测试 driver 走官方签名（503） | **部分**：服务适配通过，但工具 execute 没有闭环；driver 不是官方插件 |
| `storage-kv/v1` | 未修改官方 `dsh-storage-domain` 经 `defineDomain`/`DomainFacility` + storage-kv 后端真实往返（504）；介质落在 Rust 侧文件 | 通过（服务面） |
| `session-log/v1` + `sessionProjections` | `session-log` Rust 服务有 221-227 单测；**没有官方插件消费者**；`sessionProjections` adapter 不存在，`RPC-L2-201..208` 在 registry/代码里都找不到（文档 `m3-l2-services.md:257-264` 只列了计划） | **不通过** |

因此 G3 的"3 个 L2 service 用未修改官方插件跑通"实际是 **2 个服务有官方包痕迹，其中 1 个 execute 不闭环；
第 3 个（sessionProjections/官方 todo）完全没做**。`m3-status.md:122` 仍写"session-log/v1 + sessionProjections"，
`m3-l2-services.md:454` 写"每服务实测耗时回填 PLAN §5.1"——两者都没有交付证据。这是把
"计划中的契约"混进了"已交付服务"。

> 官方包测试本身还有一个 **P0 级** 的可信度问题，见下节。

## P0 — gate 虚假（必须先修）

### P0-1 官方包验收在缺 fixture 时静默 SKIP，conformance 仍判 pass

**证据链：**

1. `crates/compat-dsh-host/tests/official_packages.rs:80-97`：`ensure_official_tool_package()`
   在 `.tmp/dsh-tools-all/dsh-tool-working-directory` 不存在时返回 `false`；
   `spawn_official_sidecar`（`:258-270`）打印 `SKIP ...` 后返回 `None`；
   每个测试（`:356-359`、`:385-388`、`:417-420`、`:440-443`）第一行就是
   `let Some(fixture) = ... else { return; };`——**测试函数正常返回，cargo 记 passed**。
2. `tools/conformance.mjs:787-818`：cargo suite 的判定是
   `cargoPackageSeen(output, selector)`——只要 `dshr-compat-dsh-host` 的测试二进制出现在输出里，
   该包下 **62 个 ID**（含 `RPC-L2-501..505`）全部 `pass`，不检查具体测试函数是否真的执行了断言。
3. CI `.github/workflows/conformance.yml:60,64` 只跑 `cargo test --workspace` 和
   `--quick`，不 vendor `node_modules`/`.tmp`（它们被 `.gitignore` 忽略）；全新 checkout/CI
   默认就是"官方包测试全 SKIP、matrix 仍绿"。
4. **本轮实测**：把 `.tmp/dsh-tools-all/dsh-tool-working-directory` 临时改名后
   `cargo test -p dshr-compat-dsh-host --test official_packages` → `5 passed; 0 failed; ... 1.00s`，
   exit 0；正常有包时是 9.90s。随后已恢复目录。
5. registry 的 `RPC-L2` group notes 已经写明"依赖 `.tmp/dsh-tools-all`，缺失时测试内部 SKIP"——
   团队知道这条限制，但 harness 没有把 SKIP 变成 gate 失败；这不是"未预料的环境噪声"，而是
   **明知可空转仍然按 pass 计**。

**影响：** G3 的官方包证据可以在**没有任何官方插件**的机器上"全绿"。这不是"测试不够强"，是
**gate 判定可被环境静默满足**。必须改成：fixture 缺失时 **fail**（或单独 gate suite 明确 skip 且
conformance 不计 pass），并在 CI vendor/downloade 官方包；否则 `RPC-L2-501..505` 不能作为 gate 证据。

## P1（应在 M3 收尾前修，或把对应 gate 降级）

| # | 文件:行 | 问题与证据 | 建议 |
|---|---|---|---|
| P1-1 | `crates/plugin-wasm/src/lib.rs:40-47`；`crates/plugin-host/src/lib.rs:116-131`；`apps/desktop-gpui/src/bin/chat.rs:6055-6057`；`docs/wasm-host.md:48`；`docs/m3-status.md:99` | **WASM 没有接入宿主/产品路径**。`plugin-host` 没有 wasm 分发，chat 对 `wasm-component` 静默 `continue`；`wasm-host.md` 自述"尚未接线"，状态却写"已集成"。安装 WASM 插件后既不加载也不报错。 | 在 `PluginHost::load_with_manifest`/profile loader 增加 wasm 分支 + `WasmPluginRuntime` 的 `Plugin` 适配；loader 对未知 runtime kind 必须报错不能 `continue`；新增 `--selftest-wasm`（安装→load→call→unload + trap 后宿主存活）；状态文档改为"引擎级隔离，未接宿主"直到完成。 |
| P1-2 | `crates/plugin-wasm/src/lib.rs:508-519`；wasmtime 50.0.1 `runtime/limits.rs:230-283` | **WASM 内存配额只限单条 linear memory**：`memory_size` 按 memory 分别生效，`table_elements` 默认不限、`tables`/`memories`/`instances` 默认 10000，代码均未设置。多 memory/大 table 可绕过 `max_memory_mb`，在实例化阶段耗尽宿主内存。 | 设置 `table_elements`、`tables`、`memories`、`instances` 的保守上限（与 manifest 内存预算联动）；加多 memory/大 table 的负面测试；或把"内存隔离"收窄为"单 memory 线性内存隔离"并改 ADR/文档。 |
| P1-3 | `crates/plugin-host/src/external.rs:777-796`、`:799-813`、`:497-498`；`:465-483`、`:535-543` | **external-process 输出/写入可打爆宿主**：`read_line` 先分配整行再检查 1MiB 帧上限；stdout 走无界 channel；`exchange` 在 deadline 之前 `write_all` 到 child stdin，子进程不读就永久阻塞。G3 的"输出超限隔离/宿主存活"在这两条路径上不成立。 | 用有界 reader（`take(MAX_FRAME_BYTES+1)` 或自写按字节 cap）在分配前截断；channel 改 bounded；`write_all` 放到带 deadline 的 watchdog（或独立写线程）；加"巨大单行/不读 stdin"故障注入测试。 |
| P1-4 | `crates/profile/src/install.rs:95-100`；`docs/plugin-authoring.md:96,267`；`templates/external-tool-plugin/Cargo.toml:20` | **安装非原子 + SDK 不可分发**：`install_dir` 先 `remove_dir_all(destination)` 再 `copy_dir`，复制失败会把已安装版本清空；模板是 repo 绝对路径依赖，SDK 未发布，"仓库外只用 semver SDK"不成立。 | 安装到临时目录 + 原子 rename（失败保留旧版本）；发布 `dshr-plugin-sdk` 0.1.x（或 vendored SDK 包）并让模板用版本依赖；把 G2 改成"内部作者 Preview"直到发布。 |
| P1-5 | `docs/m3-l2-services.md:13,206-269,454`；`crates/compat-dsh-host/src/services.rs:236`；`driver` 只 inject `workingDirectory`/`storage`；registry 无 `RPC-L2-201..208` | **sessionProjections 未实现**，session-log 无官方插件消费者；G3 的 3 个 L2 服务实际只有 2 个有官方包证据（且 working-directory execute 不闭环）。文档把 201-208 写成已计划/已覆盖。 | 明确把首批服务改为 working-directory + session-log + storage-kv，或补 `sessionProjections` adapter + 未修改 `dsh-tool-todo` 的 201-208；把 `m3-l2-services.md` 的"conformance"表标成 pending，并在 gate evidence 里写清 2/3。 |
| P1-6 | `crates/compat-dsh-host/tests/official_packages.rs:80-97`；`tools/conformance.mjs:787-818`；`.github/workflows/conformance.yml:60,64` | **P0-1 的实现面**：SKIP 被当 pass，CI 不 vendor 官方包；`--quick` 只验证 34/476，`--workspace` 也只 442/476，仓库里没有一个提交的 full matrix。 | fixture 缺失 fail；conformance 增加 per-test 判定（或至少对 `official_packages` 用 `--exact` 测试名并解析 `test ... ok`）；CI 增加 `--full` 与 vendor 步骤；上传 full matrix 而不是 quick。 |
| P1-7 | `crates/compat-dsh-host/src/lib.rs:780-799`、`:886-896`；单调用 `:665-684`、`:920-960` | **批量取消语义与单调用不一致**：单调用在 cancel 后只给 2s 宽限，随后返回 `Cancelled`；`invoke_batch` 发一次 `tool.cancel` 后就一直等到 per-call deadline（默认 120s）或终态。sidecar 忽略 cancel 时，用户点"停止"可能被拖 120s。 | 批内每个 slot 记录 cancel 时间，给 2s 宽限后以 `Cancelled` 结算并继续/退出；加"sidecar 忽略 cancel"的 batch 测试；UI 停止路径覆盖。 |

## P2（M4 前修，或写进已知限制）

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| P2-1 | `docs/conformance-matrix.md` 当前内容；`.github/workflows/conformance.yml:64` | **提交的 matrix 是 quick-only**：`implemented passed=34`、`skipped=442`，cargo/WASM/L2/并行全 SKIP；状态却引它作 gate 证据。`--full` 本轮可 476/476，但没有提交的 full matrix 产物。 | 提交 `--full` 矩阵（含 invocation/环境）；或明确"matrix 只是契约/文链门禁，cargo 以 CI 日志为准"，两者别混。 |
| P2-2 | `apps/desktop-gpui/src/bin/chat.rs:6250-6360`；`crates/ui-gpui/tests/adc_perf.rs` | **ADC panel 产品面只验证到 `AnyElement` 构造**：selftest 拿到 element 后立即 drop，没有窗口/布局/paint；预算测试也只测 parse+view。 | 加无头/最小窗口的 draw 冒烟（或 GPUI 的 element tree 断言 + 渲染快照），至少证明 panel 真被布局一次；文档把"已接产品渲染"改成"构造链路已接，窗口验证 M4"。 |
| P2-3 | `crates/plugin-host/src/lib.rs:96-104`；`crates/plugin-api/src/lib.rs:201-203` | **panel id 全局冲突未拒绝**：`self.panels.insert(panel.id, ...)` 直接覆盖；`register_panel` 不做重复检查。恶意/粗心插件可用同一 id 顶掉别的插件面板。 | `load` 前检查全局唯一（与 tool 一样返回 `Duplicate`）；panel 注册走 `ui-contributions` 的 slot/key 域校验。 |
| P2-4 | `apps/desktop-gpui/src/bin/chat.rs:7373-7375` | `--selftest-l2` 用 `std::env::temp_dir()` 且忽略 `create_dir_all` 失败；在用户 `%TEMP%` 不可写的会话里 phase B 直接 `E_INTERNAL`，而项目其它 selftest 已切到 data root。 | 用 data_root 下的临时子目录（与其它 selftest 一致）；`create_dir_all` 失败要打印真实路径/原因；文档注明 build-env 的 TEMP 前提。 |
| P2-5 | `crates/profile/src/trust.rs:334-337`；`tools/plugin-pack.mjs:174`；`docs/spec/plugin-trust.md:151` | **摘要规范漂移**：spec T2 说 `.dshr-install.json` 不参与摘要，Rust/Node 都把它算进去；Rust 跳过**任意层级**名为 `dshr-trust.json` 的文件，Node 只跳过根级——带嵌套 trust 文件的包两边摘要会不一致。 | 统一 spec 与两端：明确只跳过根 `dshr-trust.json` 和 `.dshr-install.json`；补一个 Rust/Node 交叉摘要单测（含嵌套/本机元数据）。 |
| P2-6 | `crates/profile/src/install.rs:97-100` | 旧版本先删后拷，复制失败/磁盘满会留下"没有任何版本"的状态；spec 说"原子替换"但实现不是。 | staging + rename；失败回滚旧目录；加模拟 copy 失败的测试。 |
| P2-7 | `crates/sidecar-guard/src/process.rs:237-246` | Job Object 在 `spawn` 之后才 assign，子进程先派生的后代可逃逸；M2 P2-7 未修。 | `CREATE_SUSPENDED` → assign job → resume（Windows）。 |
| P2-8 | `docs/plugin-authoring.md:329` | 无 `--uninstall-plugin`，卸载要求手动删目录/条目，且审计不记录卸载。 | 增加 uninstall CLI（删 node_modules + profile 条目 + 审计 `action:"uninstall"`），或明确 G2 不承诺卸载生命周期。 |
| P2-9 | `docs/m3-status.md:99`；`docs/m3-gate-evidence.md:54`；`docs/plugin-authoring.md:22` | **文档漂移**：status 说 WASM "已集成"、gate evidence 说"Panel 产品渲染未接"（W5 已接）、authoring 文档也说 panel 产品渲染"仍在后续轮次"。 | 以代码为准统一：WASM=引擎级/未接宿主；panel=构造链路已接/窗口验证 M4；删掉过时句子。 |
| P2-10 | `crates/compat-dsh-host/src/lib.rs:752`；`:642,682` | batch/单调用都把 `session_id` 写死为 `"s-probe"`，而不是当前会话；L2 的 `requires_session` 方法依赖真实 session id，批路径目前没有把 chat 的 session 传进来。 | `BatchInvokeCall`/`invoke_as*` 接受 `session_id` 参数，chat 从 `parent_id` 传入；加 session 透传测试。 |
| P2-11 | `crates/plugin-host/src/lib.rs:67-131`；`crates/plugin-api/src/lib.rs:217`；`external.rs:894-898` | `Plugin::call(&mut self)` + `PluginHost` 单 `BTreeMap` 导致 **所有 native/external 插件调用串行**；`ExternalPlugin` 每次装载 `Box::leak` 两个字符串，反复 reload 泄漏。ADR-0010 的"每插件并发 4"对调用不成立（只是 Job 进程数）。 | M4 前把 `Plugin` 改为可并发调用/内部加锁，或明确"外部插件单并发"并写进 spec；`id/version` 改为 `String`（需要 trait 变更）或接受 leak 并记录上限。 |
| P2-12 | 文档/测试多处 | **测试/证据质量**：`m3_parallel_002` 用 80ms sleep + 200ms 墙钟阈值；external 内存测试接受 `E_TIMEOUT/E_INTERNAL/E_QUOTA` 多码（`external_process.rs:353-364`）；WASM epoch 100ms；`--selftest-l2` 依赖 TEMP。负载下可能 flaky。 | 用事件/同步原语代替 wall-clock；给内存测试独立进程/更长预算但仍断言"被杀"；CI 固定 runner 并记录抖动率。 |
| P2-13 | `docs/m3-l2-services.md:454` | "每服务实测耗时回填 PLAN §5.1"没有对应表/数据；`m3-l2-services.md:257-264` 的 201-208 只在文档里。 | 补 per-service p50/p95（真实 Rust adapter + sidecar 往返）并回填；未做的 ID 标 pending/blocked。 |

## P3（低优先级/记录）

- `crates/plugin-wasm/src/lib.rs:456-463`：`dshr_alloc` 返回 0 被当合法指针；文档 `wasm-host.md:26`
  说 0 失败。统一为 `ptr <= 0` 失败（或改文档）。
- `crates/plugin-wasm/src/lib.rs:545-577`：每个 runtime 一个 epoch 线程，`spawn(...).expect(...)`；
  多插件时线程数线性增长（可接受但应在文档写）。
- `tools/conformance.mjs:210-231`：`scanTestIds` 只看注释/函数名，不验证 `#[test]` 或
  `#[ignore]`；注释标一个 ID 在任意 helper fn 上方也能"存在"。
- `--selftest-parallel-tools` 在找不到 sidecar 时 `return 0`（`chat.rs:7457-7461`），与其它
  selftest 的 SKIP 语义不一致；manual 证据可能被误读为 PASS。
- `--selftest-dsh`/`--selftest-live-20` 的 SKIP 不是 gate 项；不要计入 M3 gate 统计。
- `crates/plugin-host/src/external.rs:497-498`：`FrameTooLarge` 在 SDK 侧有 1MiB 限制，但恶意
  子进程不走 SDK；即使做了有界读，仍需把超大帧作为 `E_TOO_LARGE` 结算并杀 Job。
- `docs/m3-status.md:116` 的"普通终端 Lead 环境 66/797/0"本轮无法在独立机器复核；本机复跑是
  66/797/0（可复现），但作者声称的"受限会话 66/796/1"也出现于首跑 exit 1 的日志噪声，需区分。

## 与 PLAN/ADR 的偏离评估

| 偏离 | 是否被正当化 | 评审判断 |
|---|---|---|
| **WASM core module（`dshr-wasm-core/v1`）而非 Component/WIT** | ADR-0010 §后果 + `wasm-host.md` 明确记录；`cpp-v1.wit` 仍是草案；测试/配额证明 core module 能做四类隔离 | **部分正当化**：作为"隔离引擎"可接受；但 PLAN §8 M3 交付物字面是 "WASM Component"，M3 没有交付 Component/WIT canonical ABI。应在 M3 收尾明确"Component 移 M4"，不能再把"WASM 主方案已交付"当完成。 |
| **WASM 未接 `plugin-host`/chat** | **没有任何 ADR 记录**；只在 `wasm-host.md:48` 的"接入建议"里写"尚未接线" | **不正当**：这不是"core module vs Component"的同一条偏离，而是"运行时存在但产品不可用"。G3 的"宿主存活/Session 无脏写"因此没有产品级证据。 |
| **官方包验收收窄为"装载绿 + 方法往返"** | `m3-gate-evidence.md:77-83` 自称"review 需确认可接受"；ADR-0007 §4 仍要求 3 个 service 用未修改官方插件跑通 | **未完成 ADR 变更**：收窄只写在 evidence 文档，不是 ADR。且实际只有 2 个服务有官方包证据、sessionProjections 未做。若接受收窄，必须改 ADR 并明确 2/3 与 execute 债务。 |
| **G2 SDK 用 path 依赖、样例在 `.tmp`** | `plugin-authoring.md:96,267` 明说"对外发布后改 semver"；`authoring-check` 证明生成/构建/打包/安装链路可跑 | **可接受的 Preview 口径，但不是 gate 字面**：G2 要求"semver SDK"；在 SDK 发布前应把 G2 标成"内部作者 Preview"，不要把"外部作者可发布"标完成。 |
| **native/MCP/spawn_child 仍串行；ADC tool card/交互/焦点 M4** | `m3-status.md:28-31,133`、ADR-0011 明示 | **可接受（非 gate）**：但 parallel 的"maxParallelToolCalls=10"实际只对 sidecar 生效；文档必须持续写"sidecar 并行"，否则会被读成全工具并行。 |
| **`event.*` Cordis 总线桥最小形态** | `m3-status.md:125-126` 标 TODO | **可接受（M4）**，但它使 sessionProjections 的官方消费路径不可用；与 P1-5 一起处理。 |

## 测试质量与证据可信度

**强项（本轮实测）：**

- `--full` 在 fixture 齐全时 476/476，schema 71/71、links 318/0、JS 13/13、Rust 66/797/0；
  `cargo test -p dshr-plugin-wasm` 20/20；authoring-check 全链路 PASS。测试数量/覆盖面在 M3
  尺度上是扎实的，尤其 ADC 解析/渲染、trust、L2 router/storage、external-process、WASM 隔离。
- 失败注入（超时/内存/崩溃/输出）和篡改/降级反例是真实反例，不是只测 happy path。

**弱项（影响 gate 判定）：**

1. **SKIP 不可见**：official_packages 5 个测试 early-return；conformance 按包判 pass。已实测
   "缺 fixture → 5 passed / 1.00s"。CI 默认就缺 fixture。
2. **conformance 的 per-ID 判定名不副实**：cargo 只匹配包名，`--quick` 只跑 34/476；提交的
   matrix 是 quick，442 个 implemented 显示 SKIP。`--full` 只是本机一次运行，没有提交产物。
3. **注释/命名扫描不能证明测试执行**：registry 校验只看 ID 是否出现在 source；一个 `#[ignore]`
   或 early-return 仍算 implemented。注释标 ID 在 helper 上方也可能被 `scanTestIds` 接受。
4. **墙钟/sleep 断言**：`m3_parallel_002`（80ms×3，阈值 200ms）、external 内存 15s 多码接受、
   WASM 100ms epoch；在 CI 共享 runner 上需要观察 flaky 率。
5. **文档数字漂移**：`m3-status` 曾写 487 ID，当前 registry 502；`m3-status` 写"矩阵 502"但
   matrix 是 quick；`m3-gate-evidence.md:54` 仍说 panel 产品渲染未接；`plugin-authoring.md:22`
   同样过期。另 `m3-status.md:115` 的"普通终端 Lead 环境 66/797/0"无法在独立环境复核。
6. **`--selftest-l2` 的环境依赖**：直接运行用户 TEMP 失败、build-env 的 `.tmp` 通过；文档只写
   "先 vendor sidecar"，没写 TEMP 前提。评审者若按 `target\debug\chat.exe --selftest-l2`
   原样跑会得到 FAIL，容易误判代码。

## M4 建议（按优先级）

1. **把 WASM 接进宿主并补齐配额**：`PluginHost`/profile loader 分发 `wasm-component`；未识别
   runtime kind 必须报错；`StoreLimits` 加 `table_elements/tables/memories/instances` 与全局并发
   16；加多 memory/大 table/巨大输出/trap 后 Session 完整性测试；新增 `--selftest-wasm`。
2. **官方包验收变成硬 gate**：fixture 缺失 fail；CI vendor 或从固定版本下载；conformance 按
   测试名而非包名判定；提交 `--full` 矩阵；`RPC-L2-501..505` 的 pass 必须附 `.tmp` 哈希/来源。
3. **external-process I/O 硬化**：有界行读、bounded channel、stdin 写 watchdog、
   `CREATE_SUSPENDED` 后 assign；加"巨大单行/不读 stdin/秒退/僵尸子进程"测试。
4. **G2 诚实化**：发布 SDK（或 vendored `dshr-plugin-sdk` 包）后再宣称"仓库外 semver"；
   安装改 staging+rename；增加 `--uninstall-plugin`；更新 authoring 文档的 panel 状态。
5. **L2 补齐**：sessionProjections adapter + 未修改 `dsh-tool-todo` 装载/投影测试；补 201-208；
   per-service 耗时表回填 §5.1；`event.*` 总线桥；把 session-log 从"仅数据面"写清楚。
6. **ADC 产品面**：tool card renderer、交互/事件/焦点（ADR-0011 降级清单）；panel 的窗口/布局
   冒烟；panel id 全局唯一与 12-slot 注册表接入。
7. **并行收口**：batch cancel 2s 宽限、`tool.chunk` 消费、session id 透传、native 并发策略；
   把"仅 sidecar 并行"写进用户文档与测试名。
8. **文档对账**：所有 "已集成/已接/全绿" 改成可复现口径；status/gate-evidence/authoring/README
   同一事实源；矩阵生成命令写进 CI 与 §8。

## 复现命令

```powershell
# 契约与门禁
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
node tools/conformance.mjs --list
node tools/conformance.mjs --workspace
node tools/conformance.mjs --full

# Rust 全量（约 60s 测试；构建缓存后）
tools\build-env.cmd cargo test --offline --workspace --no-fail-fast

# WASM 隔离单跑
tools\build-env.cmd cargo test --offline -p dshr-plugin-wasm

# JS L2 单测
node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs

# 产品自检（先构建 bins/chat）
tools\build-env.cmd cargo build --offline -p dshr-plugin-host --bins
tools\build-env.cmd cargo build --offline -p dshr-desktop-gpui --bin chat
target\debug\chat.exe --selftest-trust-install
target\debug\chat.exe --selftest-external-plugin
target\debug\chat.exe --selftest-adc-panel
target\debug\chat.exe --selftest-l2          # 需 TEMP 可写；build-env 下会设 .tmp
target\debug\chat.exe --selftest-defaults
target\debug\chat.exe --selftest-parallel-tools
target\debug\chat.exe --selftest-dsh

# 外部作者 E2E（约 1-2 分钟）
powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1

# P0 复现：缺 fixture 时 official_packages 仍 "5 passed"
Rename-Item .tmp\dsh-tools-all\dsh-tool-working-directory dsh-tool-working-directory.review-bak
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host --test official_packages
Rename-Item .tmp\dsh-tools-all\dsh-tool-working-directory.review-bak dsh-tool-working-directory
```

`--selftest-l2` 若直接运行失败（`storage-kv: create storage root: 拒绝访问`），先用
`$env:TEMP='C:\Projects\dshr\.tmp'; $env:TMP=$env:TEMP` 复跑，以区分环境与代码——本轮两者结果
已记录在验证清单 #12/#13。

## 未验证项（明确声明）

- **release 渲染预算 3.78ms**：本轮只跑了 debug 的 `cargo test`，未复跑 release
  `adc_perf`；`docs/m3-render-budget.md` 的数字未独立核实。
- **窗口内 draw/layout**：没有 GPUI 窗口/帧探针；ADC panel 只验证到 `AnyElement`。
- **GitHub Actions**：workflow 未在真实 runner 跑；CI 的 Node 22 vs 本机 Node 24、官方包
  vendor 缺失，均未验证。
- **真实签名安装**：只有 ed25519 单测；没有发布侧签名工具/真实 key 的端到端安装。
- **真实外部作者**：没有第二台机器/无 repo checkout 的构建；path 依赖使其不可行。
- **WASM Component/WIT**：`cpp-v1.wit` 仍是草案，没有 canonical ABI/wit-bindgen 实现。
- **大规模/压力**：并发 batch 只测 3 个 2s 调用；没有 10 并发、长会话、慢盘/低内存压力。
- **LLM 真实网络**：`--selftest-live-20` 未跑（无 key）；不是 M3 gate，但 M3 默认差异的线上
  行为未验证。
- **`dsh-storage` hub 本体**：按设计未装载（Service class seam）；`dsh-storage-domain` 代表
  kv facet，hub 生命周期/迁移未验证。
