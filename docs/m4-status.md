# M4 状态 — 跨平台 Beta

> 更新：2026-11-16（W1 完成；W2 键盘/a11y 基线实现，G3 待真机复验）。
> 范围与顺序见 [ADR-0013](decisions/0013-m4-scope.md)；PLAN 基线见 [`PLAN.md`](PLAN.md) §8 M4 / §9.2；
> M3 收尾口径见 [ADR-0012](decisions/0012-m3-gate-closure.md)。
> 本文是**活文档**：只写已实测的证据，未跑/未接 runner 的项显式标"未验证"。

## 1. 当前结论

M4 已启动，W1（分发与信任生命周期）按**离线渠道口径完成**：卸载、密钥吊销/时间窗/轮换、
作者签名安装、SDK 离线发布包与消费链路均有自动化/脚本证据。
M4 整体**未完成**：W2–W8 与三平台 CI（G1）、键盘全流程（G3）、性能进 CI（G4）仍未开始；
registry/CA 证书体系按 ADR-0013 归入 W7。

## 2. 工作包进度

| # | 工作包 | 状态 | 证据 / 缺口 |
|---|---|---|---|
| W1 | 分发与信任生命周期 | **完成（离线渠道口径）** | `M4-UNINSTALL-001..008`、`M4-KEY-001..010`（cargo，dshr-profile 81 passed）；`M4-SDK-001`（SDK 离线包/校验/篡改拒绝）、`M4-SIGN-001`（签名安装/轮换）、`M4-LIFECYCLE-001`；registry/CA 证书体系归 W7 |
| W2 | 键盘与无障碍 | **部分完成（待 G3 真机复验）** | 全局键位（新建/聚焦/侧栏/插件/审批/关闭）+ 按钮 role/aria_label + TextInput role；`M4-A11Y-001` cargo；见 [m4-keyboard.md](m4-keyboard.md)。G3 人工流程 `M4-KEYBOARD-001` 未跑 |
| W3 | ADC 交互与工具卡 | **部分完成（onPress + openExternal + tool card extra + 全部表单 onChange + draw 硬预算）** | `copy`/`call`/`command`；`openExternal` 确认条；`ToolCard.extra`；Checkbox/Switch/Select/Input/Textarea onChange；Slider −/+ 步进；`frame-probe` 硬预算 PASS；`M4-ADC-001..008` cargo，`100/101` 真机待跑；见 [m4-adc.md](m4-adc.md) |
| W4 | native/MCP 并发 + trait 冻结 | **部分完成（native 跨插件批并行 + id 去 leak）** | `call_approved_batch` 按插件分组并行、同插件串行（001/002）；`Plugin::id/version` 借用；MCP、同插件内并发、异步/取消冻结仍待 |
| W5 | WASM Component/WIT | 未开始 | 当前 `dshr-wasm-core/v1` 是 core module |
| W6 | 官方 execute 闭环 | **部分完成（todo + working_directory execute 闭环）** | `exec.agent.session.append` → `session-log/v1 events.append`；official gate 6/6 PASS；其余官方工具仍需 shell/systemPrompt 等 agent 面 |
| W7 | 三平台 CI / 性能基线 | **部分完成（G4 + 三平台 matrix + 官方包来源 lock）** | `tools/perf-gate.mjs` + step/artifact；`cross-platform.yml` 三平台 contract + 19 核心 crate；`official-provenance.mjs` 校验 3 个官方包版本/SHA-256；Actions 未实跑、桌面 GPUI 未覆盖 |
| W8 | P2 余项 | **部分完成（SKIP-as-pass 修复）** | `parallel_batch` / `l2_host_services` 在 node 缺失时改为 panic（exit 101），不再 eprintln SKIP + return 记 passed；`M4-P2-001` manual；矩阵元数据/事件顺序/flaky 治理仍待 |

## 3. W1 证据（2026-11-16）

| 交付 | 文件 | 证据 |
|---|---|---|
| M4 范围/顺序/gate 口径 | [`decisions/0013-m4-scope.md`](decisions/0013-m4-scope.md) | W1–W8 与 G1–G4 一一对账；本机不可验证项标注 |
| 卸载（目录原子移除 + profile + 审计） | `crates/profile/src/uninstall.rs`、`Profile::remove_entry` | `M4-UNINSTALL-001..008`（8 tests） |
| 卸载 CLI | `chat --uninstall-plugin <name>` | 手动/脚本命令；`m4-status` 复现命令 |
| 密钥吊销/时间窗 | `TrustKey.{not_before_ms,not_after_ms,revoked,revoked_at_ms}`、`verify_package_at` | `M4-KEY-001..003/007` |
| 密钥轮换 + 原子保存 | `TrustStore::{insert,remove,revoke,rotate,save}`、`rotate_trust_key` | `M4-KEY-004..006/008`（含真实安装路径） |
| 密钥库审计 | `<profile>/trust-key-audit.jsonl` | `M4-KEY-009`（add/rotate/revoke 3 条） |
| 产品 CLI + 自检 | `chat --trust-key ...`、`--selftest-plugin-lifecycle` | 自检输出 `installed/uninstalled/key_added/old_install/rotated/store_ok/old_rejected/new_install/meta_ok/key_audit=true`，PASS |
| SDK 离线渠道 | `tools/sdk-pack.mjs`、`tools/new-plugin.mjs --sdk` | `sdk-manifest.json` + `SHA256SUMS`；`--verify` 篡改非零、`--force` 重建恢复；样例从离线包构建（authoring-check 0d） |
| 作者签名安装/轮换 | `tools/plugin-pack.mjs --generate-key/--key/--key-id` | 签名安装免 `--yes-install`；`--trust-key rotate` 后旧钥包 `E_TRUST_KEY_REVOKED`、新钥重签可安装（authoring-check 10） |
| 契约 | `schema/cpp-v1/trust-keys.schema.json` + `schema/check.mjs` 用例 | `node schema/check.mjs` → **76/76** |

实测命令与结果：

```text
tools\build-env.cmd cargo test --offline -p dshr-profile
  → 81 passed; 0 failed; 0 ignored

target\debug\chat.exe --selftest-plugin-lifecycle
  → [selftest-lifecycle] PASS（exit 0）

target\debug\chat.exe --selftest-recovery
  → corrupt/backup/continue/plugin_rollback/healthy/healthy_call 全 true，PASS（exit 0）

target\debug\chat.exe --selftest-sidecar-recovery
  → live/killed/dead_call/native 全 true、replayed=3，PASS（exit 0）

target\debug\chat.exe --selftest-crash-recovery
  → crashed/backup/before/after/notice 全 true，PASS（exit 0）

powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1
  → PASS（6/6；RPC-L2-201 todo execute 写 todo/write，RPC-L2-502 working_directory execute 返回 cwd）

node tools\perf-gate.mjs --json docs\perf-baseline.json
  → PASS（worst_total_p95=10.799ms / limit 50ms；max_quota_total_p95=4.033ms / limit 16.7ms）

python -c "import yaml; yaml.safe_load(open('.github/workflows/cross-platform.yml',encoding='utf-8'))"
  → YAML 解析 OK（contract 5 steps / core-rust 6 steps；Actions 未实跑）

node tools\official-provenance.mjs --check
  → PASS（3 个官方包，版本+SHA-256 匹配；official-l2-gate 测试前强制）

powershell -NoProfile -ExecutionPolicy Bypass -File tools\frame-probe.ps1 -Frames 120
  → HARD PASS（draw_p95=9.40ms <= 31.25ms，missed_ratio=0）；相对基线 avg_fps 19.2 < 22.42（RDP，未判回归）

powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1 -SkipBuild
  → [authoring-check] PASS（含卸载生命周期；exit 0）

powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1 -SkipBuild
  → PASS（SDK 离线包/生成/构建/安装/卸载/签名安装/密钥轮换；exit 0）

node schema\check.mjs
  → 76/76 cases passed

node tools\conformance.mjs --full --compact
  → 67 suites / 869 passed / 0 failed / 6 ignored；exit 0
    （6 ignored = official gate manual；矩阵落盘 docs/conformance-matrix.*）
```

**已知边界**：

- `verify_package`（M3 兼容入口）只检查吊销，不检查时间窗；安装路径用
  `verify_package_at` + `TrustPolicy.now_ms` 才会拒绝过期/未生效钥。这是为了不改变
  M3 调用方行为，已在 spec §4 写明。
- 轮换后旧钥立即失效，没有宽限期；作者需用新钥重签并重装。
- `--uninstall-plugin` 不做回收站/恢复；删除前只做 rename 保护，删除阶段失败会恢复原名。
- SDK 只有**离线渠道**（目录 + manifest + SHA256SUMS），没有 registry/npm；证书体系、
  自动更新与吊销分发（CA/CRL）仍属 W7。
- 私钥只存在于作者本机；`--generate-key` 生成的 `private.pem` 不得进包/提交。
- **flaky 观察**：2026-11-16 的一次 `--full` 中 `dshr-ui-gpui` 出现 1 个失败（并行负载下
  ADC 构造预算），隔离子跑与随后 `--full` 均 0 failed；归入 W8 flaky 治理，不作为通过证据。
- **G3 未自动验证**：快捷键表有单测，但真实窗口的按键分发/焦点链/IME 仍需按
  [m4-keyboard.md](m4-keyboard.md) §3 人工复验。本机 `SendKeys` 自动化尝试被 Windows 前台锁
  拦住（窗口能启动，`SetForegroundWindow` 失败，无 `[keyboard]` 行），不改判 G3。
- **W4 只去掉 id/version leak**：`Plugin::id/version` 已改为借用 `&str`，外部/WASM 插件不再
  `Box::leak`；但 `Plugin::call` 仍是同步接口，native/MCP 批内仍串行，trait 冻结（异步/取消）
  未完成，W4 不标"完成"。
- **W6 只覆盖 todo/working_directory**：最小 `exec.agent`（id + session.append）已让这两类官方
  工具的 execute 闭环；bash/pwsh/fs 等还读 `shell`/`shellEnv`/`systemPrompt`/`ctx.fiber` 等
  活对象，仍不能算"官方 execute 全部闭环"。
- **W4 只做了跨插件 native 并行**：`call_approved_batch` 让不同插件的 native 调用重叠，
  同一插件内仍串行；`Plugin::call` 仍是同步接口，MCP 未接线，异步/取消 trait 冻结未完成。
- **W3 窗口 draw 只硬预算通过**：`frame-probe` 在 RDP 32Hz 下 missed_ratio=0、
  draw_p95=9.40ms < 31.25ms；同主机相对基线 avg_fps 19.2 < 22.42 未通过，属远程合成器
  干扰，不判回归，也不标"窗口 draw 稳定达标"。
- **G1 只加了 matrix 文件**：`cross-platform.yml` 让三平台跑契约 + 19 个核心 crate，
  但 Actions 未实跑，桌面 GPUI 的 macOS/Linux 构建/打包也没有 job；G1 不标"完成"。
- **G4 门禁已落地，CI 未实跑**：`tools/perf-gate.mjs` 与 conformance.yml step 已提交，
  本机 PASS；GitHub Actions 尚未真实执行，三平台 runner 也未加，所以 G4 不标"完成"。
- **G2 只到 headless/进程级 + 恢复横幅**：三个 selftest 覆盖损坏/备份/插件回滚、kill sidecar、
  子进程 abort；恢复后会显示“已恢复：会话从备份恢复”横幅，但没有真实窗口内崩溃后继续操作的
  端到端 UI 场景；G2 不标"完成"。
- **SKIP-as-pass 已修（W8）**：`crates/compat-dsh-host/tests/{parallel_batch,l2_host_services}.rs`
  在 node 缺失时 panic。实测把 `PATH` 限制为 `C:\Windows\System32;C:\Windows` 后两个测试二进制
  exit 101（此前是 SKIP + return 记 passed）；普通环境仍全绿（parallel 10、l2 10）。
  `official_packages.rs` 的过期模块文档已改为 `#[ignore]` + 显式 gate。

## 4. Gate 对账

| Gate | 判定 | 当前状态 | 缺口 |
|---|---|---|---|
| G1 三平台 + 两类插件 | 三平台 CI 全绿；两类来源插件回归 | **未验证**（matrix 已新增） | `cross-platform.yml` 覆盖三平台契约 + 19 核心 crate（external/WASM）；Actions 未实跑；桌面 GPUI 的 macOS/Linux 构建/打包未做 |
| G2 崩溃/插件失败恢复 | 数据可恢复、可继续操作 | **部分（headless + UI 提示）**：`--selftest-recovery` / `--selftest-sidecar-recovery` / `--selftest-crash-recovery` PASS；恢复后 `plugin_errors` 生成“已恢复”横幅 | 真实窗口内崩溃/重启后继续操作的端到端 UI 未覆盖 |
| G3 键盘全流程 | 无鼠标完成主流程 | **部分**：键位与 a11y 元数据已实现，`M4-A11Y-001` 通过；真实窗口人工复验未做 | 按 [m4-keyboard.md](m4-keyboard.md) §3 跑一遍，补 `M4-KEYBOARD-001` 证据 |
| G4 性能基线进 CI | 基线 + 阈值由 CI 执行 | **部分**：`tools/perf-gate.mjs` 阈值 50ms / 满配额 16.7ms / a11y 0，本机 PASS；CI workflow 已加 step 与 artifact | Actions 未实跑；三平台 runner 未加 |

## 5. 下一步（按依赖排序）

1. **W2/W3 真机复验**：按 [m4-keyboard.md](m4-keyboard.md) §3 跑 G3；按 [m4-adc.md](m4-adc.md) §3
   点击 copy。两者都需要真实窗口，当前本机前台锁限制见 §4。
2. **W3 剩余**：Input/Select `onChange`、tool card extra、draw 预算；`openExternal` 确认条待真机复验。
3. **W7**：三平台 CI matrix、官方包 vendor 来源校验、性能基线进 CI（G1/G4 的前置）。

## 6. 复现命令

```powershell
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
tools\build-env.cmd cargo test --offline -p dshr-profile
tools\build-env.cmd cargo test --offline -p dshr-desktop-gpui --bin chat
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-recovery
powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1 -SkipBuild
```
