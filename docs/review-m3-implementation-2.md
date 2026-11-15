# M3「插件平台 Preview」实现复审（第二次独立评审，2026-11-15）

reviewer: review-m3-implementation-2（fresh context，只读评审；本文件是唯一写入产物）。
判定基准：**HEAD `5d2335d`**（评审开始时 `git status --porcelain` 为空）。评审过程中工作区出现了
**非本 review 产生的未提交改动**（`tools/conformance.mjs`、`docs/conformance.d/registry.json`、
`docs/conformance.md`，是一份 W8 conformance 同步），本 review 未回退、未提交、未修改这些文件；
凡涉及 registry/conformance 的结论均同时给出 **HEAD 事实**与**工作区未提交 delta**，避免把未提交
修复算成 HEAD 已交付。核对对象是第一份评审 [`review-m3-implementation.md`](review-m3-implementation.md)
的 P0/P1 与 ADR-0012 口径；下面每个数字都是本轮自己跑的。

## 结论（verdict）

**有条件接受（accepted with conditions）。**

- **P0 已关闭。** `official_packages` 默认 `0 passed; 6 ignored`（不是 passed）；
  `tools/official-l2-gate.ps1` 有 fixture 时 **6/6 PASS**，临时移走
  `.tmp/dsh-tools-all/dsh-tool-working-directory` 后 **exit 1**（已恢复）。gate 不再能被
  "缺 fixture"静默满足。
- **ADR-0012 的修正口径可接受**：G2 降为"内部作者 Preview"、WASM Component/WIT 明确移 M4、
  官方包证据改显式 manual gate、工具并行只对 sidecar 生效。G1/G2/G3 在新口径下**可判定**，
  且与当前实际交付基本一致。
- **P1 7 条中 6 条关闭，1 条部分关闭：**
  - 关闭：P1-1、P1-2、P1-4、P1-5、P1-7；P1-6 的"实现面"（ignore + 显式 gate）关闭。
  - **P1-3 仍开放一半**：stdout 有界读、bounded channel、stdin 写 watchdog 都已实现并有测试；
    但 **`read_stderr` 仍是无界 `read_line`**（`external.rs:914-929`，`STDERR_TAIL_LIMIT` 只限
    行数不限行长）。恶意 external-process 插件写一条超长 stderr 行仍可在分配后打爆宿主内存。
- **P1-6 在 HEAD 上仍未完全关闭**：HEAD registry 中 `RPC-L2-501..505` 仍是 group `implemented`，
  `RPC-L2-201` 未登记，`tools/conformance.mjs` 仍按包名判 pass；因此 **HEAD 的 `--workspace`
  矩阵仍会把 501..505 判 PASS**（尽管 cargo 是 ignored）。工作区未提交 delta 已把 201/501..505
  改 manual，并实测矩阵为 MANUAL；但该 delta 的 `cargoPackageIgnored` 匹配逻辑有缺陷（见 N2），
  提交前应修。
- **条件（M3 标记完成前）：**
  1. 给 `read_stderr` 加与 stdout 同级的"分配前有界"（例如 `take(MAX_FRAME_BYTES+1)` +
     逐行截断/丢弃），或把 external-process 的"输出隔离"口径明确收窄并写进 ADR/证据；
  2. 提交 conformance 同步（registry 201/501..505 → manual），并修正 `cargoPackageIgnored`
     对集成测试二进制的匹配；建议再补一条"6 ignored 时 501..505 不得 PASS"的回归；
  3. 刷新 `docs/conformance-matrix.*` 为 `--full` 产物、修正下述过期文档（P2）。
     条件 1、2 未完成前不宜把 M3 标完成；但**不再构成 gate 虚假**，因为官方包已有显式 gate。

## 状态快照与说明（重要）

- 评审开始时 HEAD `5d2335d`、工作区干净；约在评审中段，上述三个文件变为 modified。其 diff 是
  "第八轮 W8/P0 修复"的 conformance 同步（新增 ignored 判定、201/501..505 manual、session
  projections 的 21 个 node 测试等）。**不是本 review 的改动**，我也没有权限/意图去 commit。
- 因此本文对 P0/P1 的关闭判定以 HEAD 为准；对"工作区 delta 能否关闭剩余风险"单独标注。
- 环境：`cargo` 不在默认 PATH，所有 Rust 命令经 `tools\build-env.cmd`（其把 TEMP 指到 `.tmp`）；
  `chat.exe` 在评审开始时已构建（20:14）。本会话 `%LOCALAPPDATA%\dshr` 不可写，chat 回退到
  `target\debug` 数据根，属本机环境，不影响下列结论。

## 本轮验证清单（命令 + 实测）

| # | 命令 | 实测结果 |
|---|---|---|
| 1 | `cargo test --offline -p dshr-compat-dsh-host --test official_packages` | **0 passed; 0 failed; 6 ignored**，exit 0；6 个测试均带 `#[ignore = "requires .tmp/dsh-tools-all ..."]` |
| 2 | `powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1`（fixture 在） | **PASS（6 gated tests）**，`6 passed; 0 failed; 0 ignored`，9.66s，exit 0 |
| 3 | 临时把 `.tmp/dsh-tools-all/dsh-tool-working-directory` 改名后重跑 gate | **exit 1**：`FAIL: 缺少 .tmp/dsh-tools-all/dsh-tool-working-directory`；已改名恢复（`restored=True`） |
| 4 | `chat.exe --selftest-wasm` | **PASS**：`installed_ok=true loaded=true echo_ok=true panel_ok=true render_ok=true trap_isolated=true survived=true unloaded=true` |
| 5 | `cargo test --offline -p dshr-plugin-wasm` | **27 passed / 0 failed**（0.39s） |
| 6 | `cargo test -p dshr-plugin-wasm --test wasm_isolation cpp_wasm_02` | **8 passed**（含 021 多 memory、022 大 table、024 小 table） |
| 7 | `cargo test -p dshr-plugin-host --test external_process cpp_iso_018/019/026` | 各 **1 passed**（0.45s / 5.94s / 0.23s） |
| 8 | `cargo test -p dshr-plugin-host --lib cpp_iso_0` | **5 passed**（027 bounded channel、028 截断、029 帧、030 写 watchdog、031 ack） |
| 9 | `cargo test -p dshr-profile` | **63 passed / 0 failed** |
| 10 | `cargo test -p dshr-compat-dsh-host --lib rpc_l2_20` | **7 passed**（RPC-L2-202..208） |
| 11 | `cargo test -p dshr-compat-dsh-host --test parallel_batch m3_parallel_108` | **1 passed**，4.37s（落在 1.8s–5s 断言区间） |
| 12 | `node --test --test-isolation=none .../session-projections.test.mjs` | **8/8 passed**（23ms） |
| 13 | `node schema/check.mjs` | **71/71 cases passed**，exit 0 |
| 14 | `node tools/check-links.mjs` | **336 links / 0 broken / 64 files**，exit 0 |
| 15 | `node tools/conformance.mjs --quick` | registry 544 ID；implemented **42/511 通过，失败 0，跳过 469**；pending=18 manual=15；exit 0 |
| 16 | `node tools/conformance.mjs --workspace --json .tmp/... --markdown .tmp/...` | suite cargo **67 suite(s), 831 passed, 0 failed, 6 ignored**（51.50s）；整体 implemented 469/511 通过，42 skip；exit 0 |
| 17 | `chat.exe --selftest-trust-install / -external-plugin / -adc-panel / -l2 / -defaults / -parallel-tools / -dsh` | **7/7 PASS**；`--selftest-l2` 在不覆盖 TEMP 的情况下也 PASS（phase A protocol=2 / phase B roundtrip=ok） |
| 18 | `powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1` | **PASS**：生成→自包含 SDK release 构建→打包→安装 0.1.0→升级 0.2.0→降级 `E_TRUST_DOWNGRADE`→篡改 `E_INTEGRITY_MISMATCH`→未确认 exit 2 |
| 19 | 工作区 delta（未提交）下的矩阵 | `RPC-L2-201/501..505` 均为 **MANUAL**（不是 PASS/SKIP）；CARGO-UNIT-001 提示 6 ignored |

未跑：`--full` 单独一遍（schema/links/node/cargo 已分别跑，等价覆盖；full matrix 未落盘）、
release 渲染预算、真实窗口 draw、GitHub Actions、真实签名安装、独立第二台机器。

## P0 逐条关闭表

| P0 | 判定 | 证据 | 残余 |
|---|---|---|---|
| P0-1 官方包验收缺 fixture 静默 SKIP 且 cargo 记 passed | **关闭（HEAD）** | 6 个测试 `#[ignore]`；默认输出 `0 passed; 6 ignored`；`official-l2-gate.ps1` 先检查 fixture，缺失 exit 1，有 fixture 跑 `--ignored` 并校验 `>=6 passed`，且输出出现 `SKIP` 也 exit 1 | HEAD 的 conformance 仍按包判 pass（见 P1-6）；官方 gate 目前只能 manual，CI 不跑 |

**P0 复核细节：**

- `official_packages.rs:278-289` 在缺 node / vendored sidecar / `.tmp` 时已改成 `panic!`，不再
  `return`；测试函数本身有 `#[ignore]`，所以默认 cargo 不会执行也不会"绿"。
- 负向实测不是只靠 `#[ignore]`：真移走包目录后 gate 在 fixture 检查阶段 exit 1，cargo 根本没启动。
- gate 的 `expected = 6` 与测试数一致；`--ignored --nocapture` 下 6/6 都真的执行（9.66s，而非
  0.0s 的空跑），说明不是"输出里出现包名"就算过。
- 注意：`official_packages.rs:11` 的模块文档仍写"缺 fixture 时 SKIP，不失败"，与实现相反（N3）。

## P1 逐条关闭表

| P1 | 判定 | 证据与说明 |
|---|---|---|
| P1-1 WASM 未接宿主/产品 | **关闭** | `WasmPlugin`（`plugin-wasm/src/plugin.rs`）+ `PluginHost::load_wasm_plugin`（`plugin-host/src/lib.rs:139-188`）+ chat loader 按 `RuntimeKind` 分发（`chat.rs:6035-6090`）。`--selftest-wasm` 真跑安装→装载→echo→ui.render→trap→存活→卸载全 true；未知/错位 kind 走 `errors.push(...)`，不再静默 `continue`。残留的 `continue` 只用于"manifest 缺失/解析失败"等正常跳过，不移交 wasm。 |
| P1-2 WASM 配额只限单 memory | **关闭** | `lib.rs:531-538`：`memory_size + table_elements(1024) + tables(1) + memories(1) + instances(1) + trap_on_grow_failure`。`CPP-WASM-021` 多 memory、`022` 大 table 均返回 `E_QUOTA(MemoryLimit)`，`024` 小 table 正常；宿主存活。`dshr_alloc` 返回 0 现在也按 ABI 错误（`023` 断言 `non-positive`）。 |
| P1-3 external 读/写可打爆宿主 | **部分关闭（open）** | stdout：`read_stdout_impl` 用 `take(MAX_FRAME_BYTES+1)` 分配前截断（`external.rs:834-873`），并改 `sync_channel(64)`（`:290`）；stdin：独立写线程 + `enqueue_write` ack deadline（`:507-528`、`:891-912`），`CPP-ISO-019/030` 实测 5.94s 内结算并杀 Job；`018/026..029/031` 全绿。**但 `read_stderr`（`:914-929`）仍是 `BufReader::read_line(&mut String)`，无字节上限**，只有 `STDERR_TAIL_LIMIT=64` 的行数上限；一条无换行巨行会在分配后耗尽宿主内存。第一份 review 的 P1-3 明确包含 stderr，故本条不能算全关。 |
| P1-4 安装非原子 + SDK 不可分发 | **关闭** | `install.rs:126-177` 改为 staging→备份 rename→rename 到位，复制失败旧版本不动、rename 失败回滚；`M3-INSTALL-001` 用注入 copy 失败验证旧版本仍在（`install.rs:619`）。生成器把 `crates/plugin-sdk` 复制到 `<out>/sdk`，模板只写 `path = "sdk"`；SDK `Cargo.toml` 不继承 workspace、不依赖 `dshr-plugin-api`（仅 serde/serde_json/thiserror）；authoring-check 断言生成物任何文本不含 repo 绝对路径并通过。SDK 未发布 registry 的事实由 ADR-0012 降级覆盖。 |
| P1-5 sessionProjections/201..208 缺失 | **关闭** | Rust `SessionProjectionsService` + 7 个 `RPC-L2-202..208` 全绿；sidecar `session-projections.mjs` + 8 个 `L2JS-021..028` 全绿；官方 `dsh-tool-todo` 仅装载绿，`RPC-L2-201` 在（工作区）registry 标 manual，文档明确 execute 受 `exec.agent` 债务 pending。 |
| P1-6 SKIP 当 pass / conformance 按包判 | **HEAD 部分关闭；工作区 delta 可关闭但需修** | HEAD：6 ignored 已保证 cargo 不记 passed；显式 gate 已能判失败。但 HEAD registry 中 501..505 仍 `implemented`、201 未登记，`conformance.mjs` 仍 `cargoPackageSeen` 即 pass → HEAD 的 `--workspace` 矩阵仍会把 501..505 判 PASS（无 fixture 也绿）。工作区未提交 delta 把 201/501..505 改 manual，实测矩阵 MANUAL；`node-test` 也扩到 21 个。**该 delta 的 ignored 检测有 bug（N2）**，且未提交。 |
| P1-7 batch 取消无 2s 宽限 | **关闭** | `lib.rs:293` `CANCEL_GRACE=2s`；`invoke_batch` 对未完成 slot 发 `tool.cancel` 并置 `cancel_deadline`，到期以 `Cancelled` 结算（`:836-857`）。`M3-PARALLEL-108` 用忽略 cancel 的 sidecar 实测 4.37s 全 `Cancelled`（<5s，不会等 30s/120s）。 |

## 新发现

### N1（P1，仍未关闭）external-process stderr 无界读

`crates/plugin-host/src/external.rs:914-929`：

```rust
fn read_stderr(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut reader = BufReader::new(stderr);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) { ... }
```

`STDERR_TAIL_LIMIT` 只裁剪 `VecDeque` 的行数，`line` 本身无上限。恶意插件写一条
`"x" * 1GB`（无换行）会让 `read_line` 在宿主内持续扩容。这与 G3 "输出超限隔离/宿主存活"
及 P1-3 的原始要求直接冲突，且现有 `CPP-ISO-018` 只覆盖 stdout。建议与 stdout 同样
`take(MAX_FRAME_BYTES+1)`，超出即记录"stderr line truncated"并丢弃余下字节；补故障注入测试。

### N2（P1，若把工作区 delta 当作 P1-6 的关闭证据）ignored 检测匹配不到集成测试二进制

工作区 `conformance.mjs` 新增：

```js
const normalized = selector.replace(/-/g, '_')   // dshr_compat_dsh_host
entries.filter(e => e.name === normalized || e.name.startsWith(`${normalized}-`) ...)
```

但 cargo 输出的测试二进制名是 `official_packages-…`、`l2_host_services-…`、`parallel_batch-…`
（见本轮输出），没有 `dshr_compat_dsh_host-…`。实测用该正则匹配 `Running tests\official_packages-…`
得到 `name=official_packages`，与 selector 不匹配，`cargoPackageIgnored` 返回 0。也就是说
**"包内 ignored>0 → FAIL"这条新逻辑对 official_packages 实际是死代码**；当前矩阵正确是因为
registry 已把这些 ID 改 manual，而不是因为 ignored 检测生效。若 registry 再回退或别的包出现
`#[ignore]`，仍会误判 pass。建议按"测试二进制所属包"建映射（或解析 `Running ... (target/.../deps/<bin>)`
后由 registry 的 source/selector 显式声明），并加回归测试。

### N3（P2）文档漂移（有过期、也有"文档 > 实际"）

- **过期（underclaim，但会误导复核）**：`docs/wasm-host.md:47-48` 仍写 plugin-host 接入
  "尚未接线"、§5 仍列"接入与后续"；`docs/plugin-authoring.md:13` 仍写"WASM 主机 M3 尚未就绪"；
  `crates/plugin-wasm/src/lib.rs:41-48` 模块文档仍写"接入 plugin-host（本轮不接）"。
  实际 `--selftest-wasm` 已 PASS，属文档没跟上代码。
- **数字/对象过期**：`docs/m3-status.md:96` 写 `official_packages.rs（5 个）`，实际 6 个；
  `official_packages.rs:11` 仍写缺 fixture "SKIP，不失败"；HEAD registry 的 RPC-L2 group notes
  仍写"缺失时测试内部 SKIP"。
- **文档 > 实际**：`docs/wasm-host.md` §3 配额表写"并发 4（全局 16）"，但代码只有每 runtime
  `max_concurrency=4`（`lib.rs:95,415`），没有全局 16 的实现；
  `docs/m3-gate-evidence.md:13` 的 G3 仍写"3 个 L2 service 用未修改官方包跑通并回填耗时"，
  实际 ADR 已改为 3+sessionProjections 第四个适配器，且 `m3-l2-services.md` 的 per-service 耗时
  仍没有回填表。
- **矩阵**：`docs/conformance-matrix.*` 仍是 e10ba8f/quick（34 passed、442 skip、registry 502），
  与当前 544/511 不一致；ADR-0012 §4 要求 gate 证据跑 `--full` 并提交 full matrix，仍未落盘
  （HEAD 与工作区都未提交新矩阵）。

### N4（P2）官方 gate 缺 fixture 来源/摘要校验

`official-l2-gate.ps1:31-35` 只检查两个目录存在，未校验官方包来源、版本或哈希；一份"形状正确"
的自制包也可能让 6 个测试全绿。第一份 review 建议的 `.tmp/dsh-tools-all` 哈希/来源记录仍未做。
manual gate 可以接受，但证据里应写清抽取来源（本机 DSH 版本/时间/哈希）。

### N5（P2）"SKIP 当 pass"模式仍存在于其它 fixture 依赖测试

`parallel_batch.rs:296-310` 的 `spawn_batch_fixture()` 在 node 缺失或 spawn 失败时打印
`SKIP` 并返回 `None`，测试第一行 `let Some(...) else { return; }` → cargo 记 passed；
`l2_host_services.rs` 的 401..410 在 node 缺失时同样 `return`。ADR-0012 §4 的硬规则
（依赖外部 fixture 的测试缺失时必须 `#[ignore]` 或失败）没有推广到这些 suite。当前 CI 有 node，
且 probe-sidecar 已提交，风险低于官方包 P0，但建议统一成 `#[ignore]` + 显式 gate/前置检查。

### N6（P3）registry 重复 ID / 201 登记不一致

工作区 delta 同时保留 group 的 `ids`（含 501..505）和显式 `tests` 的 manual 501..505，
形成同一 ID 两条记录；当前 loader 让显式 manual 胜出，矩阵正确，但 registry 卫生差，
后续工具若改为"group 覆盖 tests"会立刻回退成 pass。HEAD 则相反：`RPC-L2-201` 有测试函数
但 registry 完全没有该 ID（首个 review 的 P1-5 证据缺失仍在 HEAD 留有痕迹）。

### N7（P3）ADR 术语

ADR-0012 写 `StoreLimits` 的 `memories/tables/table_instances/instances`，代码实际是
`tables` + `table_elements`（wasmtime 的字段名）。不影响行为，但会误导实现者。

## Gate 判定（按 ADR-0012 修正口径）

| Gate | 判定 | 依据 | 备注 |
|---|---|---|---|
| **G1 ADC-v1 冻结** | **通过**（维持） | spec `Frozen`、71/71 schema、26 元素、未知元素 Placeholder + 诊断、`--selftest-adc-panel` PASS | 本轮未重跑 ADC 全量；窗口 draw/layout 仍属 ADR-0011 的 M4 降级，不算 gate 失败 |
| **G2 外部作者可发布** | **有条件通过（内部作者 Preview）** | authoring-check 全链路 PASS；SDK 自包含、样例 `path = "sdk"`、无 repo 绝对路径；安装原子、信任/降级/篡改/未确认反例全过 | 未发布 registry、无 `--uninstall-plugin`、无真实签名安装；按 ADR-0012 不得对外称"可发布"。另：authoring-check 的构建仍发生在 repo 内 `.tmp`，"仓库外"靠静态路径检查 + 独立 `[workspace]` 推断，未真的在 repo 外目录执行 |
| **G3 隔离** | **通过（core-module 口径，附 N1 条件）** | WASM 已接宿主/产品：`WasmPlugin` + loader + `--selftest-wasm` 全 true；27 个 WASM 测试（含 021/022 资源上限）；external-process 有界 stdout/channel/stdin watchdog 且 018/019/026..031 全绿 | **stderr 仍无界（N1）**；`Component/WIT` 已按 ADR 移 M4，M3 不声称 Component |
| **G3 L2 兼容** | **通过（manual gate 口径）** | `official-l2-gate.ps1` 有 fixture 6/6 PASS、缺 fixture exit 1；working-directory 装载/execute 债务、dsh-storage-domain 往返、workingDirectory 签名往返、storage hub Service class 均有测试 | HEAD 的 conformance 仍会把 501..505 判 PASS（P1-6）；工作区 delta 改 manual 后矩阵正确。官方 execute 闭环仍是 `exec.agent` 债务（已文档化，M4） |

**总体**：ADR-0012 之后，G1/G2/G3 都有可复现的判定路径；"文档声称 > 实际"的主要项已收敛到
N3 的少数过期句/全局并发数字；没有再发现 P0 级虚假 gate。**可以按"有条件接受"收尾**，条件即
N1 + P1-6 的提交/修正 + full matrix；这些不满足时不应把 M3 标完成。

## 复现命令

```powershell
# P0：默认必须 0 passed / 6 ignored
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host --test official_packages

# P0：显式官方 gate（有 fixture → 6/6 PASS）
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1

# P0 负向（务必恢复；.tmp/dsh-tools-all 本身可能是 junction，改子目录名）
Rename-Item .tmp\dsh-tools-all\dsh-tool-working-directory dsh-tool-working-directory.review2-bak
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1   # 期望 exit 1
Rename-Item .tmp\dsh-tools-all\dsh-tool-working-directory.review2-bak dsh-tool-working-directory

# P1
tools\build-env.cmd cargo test --offline -p dshr-plugin-wasm
tools\build-env.cmd cargo test --offline -p dshr-plugin-wasm --test wasm_isolation cpp_wasm_02
tools\build-env.cmd cargo test --offline -p dshr-plugin-host --test external_process cpp_iso_018
tools\build-env.cmd cargo test --offline -p dshr-plugin-host --test external_process cpp_iso_019
tools\build-env.cmd cargo test --offline -p dshr-plugin-host --test external_process cpp_iso_026
tools\build-env.cmd cargo test --offline -p dshr-plugin-host --lib cpp_iso_0
tools\build-env.cmd cargo test --offline -p dshr-profile
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host --lib rpc_l2_20
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host --test parallel_batch m3_parallel_108
node --test --test-isolation=none sidecar/dsh-official/lib/session-projections.test.mjs
target\debug\chat.exe --selftest-wasm

# 回归与 gate
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
tools\build-env.cmd cargo test --offline --workspace --no-fail-fast
powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1
target\debug\chat.exe --selftest-trust-install
target\debug\chat.exe --selftest-external-plugin
target\debug\chat.exe --selftest-adc-panel
target\debug\chat.exe --selftest-l2
target\debug\chat.exe --selftest-defaults
target\debug\chat.exe --selftest-parallel-tools
target\debug\chat.exe --selftest-dsh
```

说明：`cargo` 不在默认 PATH，`build-env.cmd` 会把它加进去并把 TEMP 指到 `.tmp`；chat 的
`--selftest-l2` 本轮在**不覆盖 TEMP**的情况下也 PASS（P2-4 已修）。`--selftest-wasm` 会打印
"数据目录不可用，回退到 target\debug"，是本机 `%LOCALAPPDATA%` 权限问题，不是产品失败。

## 残余风险 / 未验证项

- **stderr 巨行攻击未做故障注入**：N1 是读代码确认的；没有用"stderr 1GB 无换行"实测，
  因为会真的尝试耗尽宿主内存。修好后应补 `CPP-ISO-032` 级别的测试。
- **工作区 delta 非 HEAD**：conformance 同步（manual 201/501..505、ignored 判定、21 个 node
  测试）未提交；本 review 的 P1-6 结论依赖"把它提交并修 N2"。若产品只按 HEAD 出包，
  HEAD 的矩阵仍会在无 fixture 时把 501..505 判 PASS。
- **CI 未验证**：`.github/workflows/conformance.yml` 没有跑过 GitHub Actions，也没有官方包
  vendor 步骤；manual gate 只能在有本机 DSH 抽取物的环境执行。CI 默认只有 `--quick`，
  其 42/511 的"绿"不代表 cargo/L2 证据。
- **full matrix 未提交**：ADR-0012 §4 要求 `--full` 落盘；当前 `docs/conformance-matrix.*`
  仍是旧 quick。本 review 用 `--workspace` + 分 suite 复现了 469/511，但没有生成提交产物。
- **release 渲染预算/窗口 draw**：未复跑 release `adc_perf`，没有 GPUI 窗口/帧探针。
- **真实签名安装 / 第二台机器外部作者**：只有 unsigned E2E 与 ed25519 单测；SDK 未发布，
  无法验证"无 repo 的第二台机器"。
- **WASM Component/WIT**：`cpp-v1.wit` 仍是草案，M3 只交付 core-module ABI（ADR 已记录）。
- **压力/长会话**：并发只测 3 个 2s 调用与 batch 108 的 2 个 10s 调用；没有 10 并发、
  慢盘/低内存/长会话压力。
- **官方 gate 的来源可信度**：见 N4，未校验 `.tmp/dsh-tools-all` 的版本/哈希。
- **`dsh-storage` hub 本体**：仍按设计不装载（Cordis Service class），由
  `dsh-storage-domain` 代表；hub 生命周期/迁移未验证。

---

**本文件是本 review 唯一写入产物。** 评审期间出现的 `tools/conformance.mjs`、
`docs/conformance.d/registry.json`、`docs/conformance.md` 修改不是本 review 所为，未回退、
未提交；如需以工作区 delta 为最终交付，请按 N2 修正后再提交。
