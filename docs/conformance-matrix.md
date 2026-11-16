# Conformance Matrix（自动生成）

> 本文件由 `tools/conformance.mjs` 生成，请勿手改；测试清单事实源是 `docs/conformance.d/registry.json`。
> **CI 状态**：`.github/workflows/conformance.yml` 尚未在 GitHub Actions 实跑；本报告由本机命令生成。
> **格式**：compact（逐 ID 行只保留 ID/领域/状态/结果/证据/消息，title/notes 见注册表）。

## 元数据

| 字段 | 值 |
|---|---|
| generated_at | 2026-11-16T18:41:48.344Z |
| git HEAD | fa5130c1865dd78fc2f26513744cb044f3c240a4（clean） |
| git branch | master |
| platform | win32 10.0.26200 x64 |
| node | v24.14.0 |
| invocation | full — `node tools/conformance.mjs --full --compact --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md` |
| registry | docs/conformance.d/registry.json（schema_version=1，621 条测试） |

## 统计

| 指标 | 数量 |
|---|---:|
| 总测试 ID | 621 |
| implemented | 566 |
| implemented passed | 566 |
| implemented failed | 0 |
| implemented skipped | 0 |
| pending | 18 |
| manual | 37 |
| blocked | 0 |
| **failed（导致退出码 1）** | **0** |

## Suite 结果

| suite | 状态 | 退出码 | 耗时 | 命令 | 摘要 |
|---|---|---:|---:|---|---|
| schema | pass | 0 | 0.23s | `node schema/check.mjs` | 76/76 cases passed |
| links | pass | 0 | 0.28s | `node tools/check-links.mjs` | 408 link(s) checked, 0 broken, 75 file(s) scanned |
| node-test | pass | 0 | 0.30s | `node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs sidecar/dsh-official/lib/session-projections.test.mjs` | 21/21 node tests passed |
| cargo | pass | 0 | 54.76s | `tools\build-env.cmd cargo test --offline --workspace --no-fail-fast` | 71 suite(s), 892 passed, 0 failed, 8 ignored；未执行: official_packages(8 ignored) |

## 测试矩阵

### 非 PASS（逐 ID 明细）

| ID | 领域 | 状态 | 结果 | 证据 | 消息 |
|---|---|---|---|---|---|
| ADC-BIND-001 | ADC | pending | PENDING | — | 依赖 adc-v1 绑定/动作实现；断言见 review-m3-plan.md §6 ADC-BIND-*。 |
| ADC-PANEL-001 | ADC | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-adc-panel：装载 fixture 插件→panels→render_panel（验证到 AnyElement 构造层）→未知元素 view；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
| CPP-AUTHOR-001 | CPP | manual | MANUAL | — | tools/authoring-check.ps1；仓库外按 docs/plugin-authoring.md + templates 构建/打包/安装（G2），人工/脚本判定。 |
| CPP-ISO-022 | CPP | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-external-plugin（安装→装载→调用→卸载→重载）；需构建 chat 二进制后运行，harness 尚无 selftest suite，人工/脚本判定。 |
| CPP-TRUST-019 | CPP | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-trust-install；需构建 chat 二进制后运行（harness 尚无 selftest suite），对应 M3 G2 信任链证据，人工/脚本判定。 |
| CPP-WASM-100 | CPP | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-wasm：安装→装载→调用→trap→卸载（review P1-1）；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
| RPC-L2-201 | RPC | manual | MANUAL | — | 需 .tmp/dsh-tools-all + vendored sidecar；command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`。M4 W6：execute 经 exec.agent.session.append → session-log/v1 events.append 写入 todo/write；gate 8/8 PASS。缺 fixture 时 gate exit 1；默认 cargo 中 #[ignore]。 |
| RPC-L2-411 | RPC | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-l2：phase A 真实 sidecar protocol=2 握手+host_services，phase B fixture storage-kv 往返；需构建 chat 二进制（phase A 还需 vendored 官方 sidecar），harness 无 selftest suite，人工/脚本判定。 |
| RPC-L2-501 | RPC | manual | MANUAL | — | 需 .tmp/dsh-tools-all + vendored sidecar；command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`。缺 fixture 时 gate exit 1；默认 cargo 0 passed/8 ignored（不计为通过）。 |
| RPC-L2-502 | RPC | manual | MANUAL | — | M4 W6：最小 exec.agent facade 后，未修改官方工具的 execute 真实返回 cwd，不再报 requires an agent Session；gate 8/8 PASS。command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`。 |
| RPC-L2-503 | RPC | manual | MANUAL | — | 同 RPC-L2-501；command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`；默认 cargo 中 #[ignore]。 |
| RPC-L2-504 | RPC | manual | MANUAL | — | 同 RPC-L2-501；command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`；默认 cargo 中 #[ignore]。 |
| RPC-L2-505 | RPC | manual | MANUAL | — | 同 RPC-L2-501；command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`；默认 cargo 中 #[ignore]。 |
| RPC-L2-506 | RPC | manual | MANUAL | — | M4 W6：未修改官方 fs 工具的 read 方法经 sidecar fs/systemPrompt seam 真实读取 fixture 文件；gate 8/8 PASS。command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`；默认 cargo 中 #[ignore]。 |
| RPC-L2-507 | RPC | manual | MANUAL | — | M4 W6：未修改官方 fs 工具（write/edit/applyPatch 配置）经 Rust FsWriteService 审批后真实落盘；测试自动 AllowOnce；gate 8/8 PASS。command=`powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`；默认 cargo 中 #[ignore]。 |
| M3-DEFAULT-070 | M3 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-defaults：无网络自检 approval 默认/read-only/重复工具软提醒阈值/默认差异接线；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
| M3-PARALLEL-201 | M3 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-parallel-tools：sidecar 批 invoke 串行 6.1s→并行 2.0s（ratio 0.33）且事件顺序约束成立；需构建 chat 二进制 + vendored sidecar，harness 无 selftest suite，人工/脚本判定。 |
| M4-ADC-100 | M4 | manual | MANUAL | — | 真实窗口：装载带 Button{onPress:copy} 的插件面板，点击后剪贴板得到文本；Button{onPress:call} 经 ui.action 到 fixture，下一次 render 的计数器 +1。需构建 chat.exe + external fixture；openExternal/onChange 仍未接。 |
| M4-ADC-101 | M4 | manual | MANUAL | — | 真实窗口：Button{onPress:openExternal:https://...} 点击后出现确认条；取消/Esc 不打开，打开才调用系统浏览器；非 https 在作者期/解析期即拒绝。需构建 chat.exe + external fixture。 |
| M4-CI-001 | M4 | manual | MANUAL | — | `.github/workflows/cross-platform.yml`：contract（schema/links/quick）、core-rust（19 个核心 crate）、desktop-build（dshr-desktop-gpui + Linux/macOS ADC/recovery/crash 自检 + 打包 + verify-package）在 windows/ubuntu/macos 三平台跑。YAML 已用 PyYAML 本地解析；Linux desktop-build 与自检/打包/验证本机通过；Actions 未实跑，G1 不标完成。 |
| M4-CI-002 | M4 | manual | MANUAL | — | node tools/official-provenance.mjs --check：3 个官方包（working-directory/todo/storage-domain）版本 + package.json/lib/index.js SHA-256 对照 sidecar/dsh-official/official-packages.lock.json；official-l2-gate.ps1 在测试前强制校验。本机 PASS，gate 8/8。 |
| M4-CI-003 | M4 | manual | MANUAL | — | WSL Ubuntu 22.04 + Rust 1.99.1：核心 6 crate cargo check 通过；dshr-desktop-gpui --bin chat cargo build 通过（BUILD_EXIT=0，debug 682 MB）；chat --selftest-adc-panel / --selftest-recovery / --selftest-crash-recovery 均 PASS。无需额外系统开发包。macOS/Windows 桌面 runner 未验证；见 docs/m4-linux.md。 |
| M4-CI-004 | M4 | manual | MANUAL | — | tools/package-desktop.ps1（Windows）与 tools/package-desktop.sh（Linux/macOS）：cargo build chat + runtime/node + probe sidecar + dsh-official（含 node_modules）+ README + 归档 + .sha256；本机 Windows zip 59.7MB、Linux tar.gz 197.1MB，均 exit 0。解压后不设 DSHR_NODE 直接 chat --selftest-sidecar-recovery / --selftest-official PASS（Vendored node，官方 v2 握手 host_services=3）。CI desktop-build 已加打包/校验步骤；不含安装器/签名/自动更新；见 docs/m4-package.md。 |
| M4-CI-005 | M4 | manual | MANUAL | — | tools/verify-package.ps1/.sh：校验 .sha256、解压到临时目录、跑 --selftest-sidecar-recovery 与 --selftest-official、清理。Windows/Linux 本机均 PASS（sha256 + 两项自检）。 |
| M4-DRAW-001 | M4 | manual | MANUAL | — | tools/frame-probe.ps1 -Frames 120：RDP 32Hz 下 draw_p95=9.40ms <= 31.25ms、missed_ratio=0（HARD PASS）；相对基线 avg_fps=19.2 < 22.42，环境原因未通过，不判回归。见 docs/m4-draw-budget.md。 |
| M4-KEY-100 | M4 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --trust-key：本机 trust-keys.json 管理与 trust-key-audit.jsonl；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
| M4-KEYBOARD-001 | M4 | manual | MANUAL | — | 真实窗口人工/UI 自动化：Ctrl/⌘+N 新建、Ctrl/⌘+K 聚焦输入、Enter 发送、Ctrl/⌘+Shift+A/D 审批、Ctrl/⌘+B/J 切侧栏/插件面板、Esc 关闭面板、Ctrl/⌘+W 关闭；chat --selftest 不驱动真实键盘事件，必须在 GUI 复验。 |
| M4-LIFECYCLE-001 | M4 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --selftest-plugin-lifecycle：安装→卸载（目录/profile/审计）→旧钥签名安装→rotate→旧钥 E_TRUST_KEY_REVOKED→新钥重签安装；需构建 chat 二进制，人工/脚本判定。 |
| M4-OFFICIAL-001 | M4 | manual | MANUAL | — | chat --selftest-official：从 app_root()/sidecar/dsh-official 启动 vendored 官方 sidecar，断言 protocol=2、host_services_active=true、3 张表；Windows/Linux 解压包均 PASS。用于验证 zip/tar 自带 dsh-official/node_modules 与 runtime/node。 |
| M4-P2-001 | M4 | manual | MANUAL | — | 把 PATH 限制为 Windows 系统目录后运行 parallel_batch/l2_host_services 测试二进制：现在 exit 101 / panic "node is required..."（修复前 eprintln SKIP + return 记 passed）。普通环境仍全绿；见 m4-status §4。 |
| M4-PERF-001 | M4 | manual | MANUAL | — | node tools/perf-gate.mjs --json docs/perf-baseline.json：10 树 worst_total_p95≤50ms、满配额树 total_p95≤16.7ms、a11y 缺失=0；CI conformance.yml 已加 step 并上传 perf-baseline artifact，但 Actions 尚未实跑。本机 PASS。 |
| M4-RECOVERY-001 | M4 | manual | MANUAL | — | chat --selftest-recovery：损坏会话隔离+干净重建、删主文件后 .bak 恢复并可继续追加、失败插件注册回滚+健康插件装载/调用。headless PASS；真实窗口崩溃恢复未覆盖。 |
| M4-RECOVERY-002 | M4 | manual | MANUAL | — | chat --selftest-sidecar-recovery：先 probe_echo 成功，再 kill sidecar；已死调用返回错误、会话仍可追加/保存、probe_native 仍注册。headless PASS；真实窗口内 sidecar 崩溃/重启未覆盖。 |
| M4-RECOVERY-003 | M4 | manual | MANUAL | — | chat --selftest-crash-recovery：子进程 save_atomic 两次后删主文件并 abort；父进程恢复 .bak、校验 before crash、追加 after crash 并保存；恢复后 plugin_errors 含“已恢复”横幅。PASS（crashed/backup/before/after/notice 全 true）。 |
| M4-SDK-001 | M4 | manual | MANUAL | — | tools/authoring-check.ps1 的 0d 步：sdk-pack 生成 sdk-manifest.json + SHA256SUMS，--verify 通过；篡改 SDK 后 verify 非零；--force 重建后恢复；new-plugin --sdk 消费离线包构建。需 node + 已构建 chat.exe。 |
| M4-SIGN-001 | M4 | manual | MANUAL | — | tools/authoring-check.ps1 第 10 步：--generate-key → --trust-key add → plugin-pack --key/--key-id → chat --install-plugin 免 --yes-install → --trust-key rotate → 旧钥包 E_TRUST_KEY_REVOKED → 新钥重签安装；需构建 chat.exe。 |
| M4-UNINSTALL-100 | M4 | manual | MANUAL | — | apps/desktop-gpui/src/bin/chat.rs 的 --uninstall-plugin：目录原子移除 + profile 条目 + install-audit 的 action=uninstall；需构建 chat 二进制，harness 无 selftest suite，人工/脚本判定。 |
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

### PASS（逐 ID；共 566 条）

| ID | 结果 |
|---|---|
| SCHEMA-FIXTURE-001 | PASS |
| SCHEMA-FIXTURE-002 | PASS |
| SCHEMA-FIXTURE-003 | PASS |
| SCHEMA-FIXTURE-004 | PASS |
| SCHEMA-FIXTURE-005 | PASS |
| SCHEMA-FIXTURE-006 | PASS |
| SCHEMA-FIXTURE-007 | PASS |
| SCHEMA-FIXTURE-008 | PASS |
| SCHEMA-FIXTURE-009 | PASS |
| SCHEMA-FIXTURE-010 | PASS |
| SCHEMA-FIXTURE-011 | PASS |
| SCHEMA-FIXTURE-012 | PASS |
| SCHEMA-FIXTURE-013 | PASS |
| SCHEMA-FIXTURE-014 | PASS |
| SCHEMA-FIXTURE-015 | PASS |
| SCHEMA-FIXTURE-016 | PASS |
| SCHEMA-FIXTURE-017 | PASS |
| SCHEMA-FIXTURE-018 | PASS |
| SCHEMA-FIXTURE-019 | PASS |
| SCHEMA-FIXTURE-020 | PASS |
| DOC-LINK-001 | PASS |
| CARGO-UNIT-001 | PASS |
| CARGO-UNIT-002 | PASS |
| CARGO-UNIT-003 | PASS |
| CARGO-UNIT-004 | PASS |
| ADC-ELEM-001 | PASS |
| ADC-ELEM-002 | PASS |
| ADC-ELEM-003 | PASS |
| ADC-ELEM-004 | PASS |
| ADC-ELEM-005 | PASS |
| ADC-ELEM-006 | PASS |
| ADC-ELEM-007 | PASS |
| ADC-ELEM-008 | PASS |
| ADC-ELEM-009 | PASS |
| ADC-ELEM-010 | PASS |
| ADC-ELEM-011 | PASS |
| ADC-ELEM-012 | PASS |
| ADC-ELEM-013 | PASS |
| ADC-ELEM-014 | PASS |
| ADC-ELEM-015 | PASS |
| ADC-ELEM-016 | PASS |
| ADC-ELEM-017 | PASS |
| ADC-ELEM-018 | PASS |
| ADC-ELEM-019 | PASS |
| ADC-ELEM-020 | PASS |
| ADC-ELEM-021 | PASS |
| ADC-ELEM-022 | PASS |
| ADC-ELEM-A11Y-001 | PASS |
| ADC-ELEM-A11Y-002 | PASS |
| ADC-ELEM-A11Y-003 | PASS |
| ADC-ELEM-A11Y-004 | PASS |
| ADC-ELEM-ALIGN-001 | PASS |
| ADC-ELEM-CHART-001 | PASS |
| ADC-ELEM-CHART-002 | PASS |
| ADC-ELEM-GRID-001 | PASS |
| ADC-ELEM-GRID-002 | PASS |
| ADC-ELEM-ICON-001 | PASS |
| ADC-ELEM-JUSTIFY-001 | PASS |
| ADC-ELEM-LIST-001 | PASS |
| ADC-ELEM-NUM-001 | PASS |
| ADC-ELEM-PANEL-001 | PASS |
| ADC-ELEM-PANEL-002 | PASS |
| ADC-ELEM-PANEL-003 | PASS |
| ADC-ELEM-PANEL-004 | PASS |
| ADC-ELEM-PANEL-005 | PASS |
| ADC-ELEM-PANEL-006 | PASS |
| ADC-ELEM-PANEL-007 | PASS |
| ADC-ELEM-PANEL-008 | PASS |
| ADC-ELEM-PANEL-009 | PASS |
| ADC-ELEM-PANEL-010 | PASS |
| ADC-ELEM-PANEL-API-001 | PASS |
| ADC-ELEM-PANEL-API-002 | PASS |
| ADC-ELEM-PANEL-HOST-001 | PASS |
| ADC-ELEM-PANEL-HOST-002 | PASS |
| ADC-ELEM-PANEL-HOST-003 | PASS |
| ADC-ELEM-PANEL-HOST-004 | PASS |
| ADC-ELEM-PLACEHOLDER-001 | PASS |
| ADC-ELEM-RENDER-001 | PASS |
| ADC-ELEM-RENDER-002 | PASS |
| ADC-ELEM-RENDER-003 | PASS |
| ADC-ELEM-SLIDER-001 | PASS |
| ADC-ELEM-SPACING-001 | PASS |
| ADC-ELEM-SPACING-002 | PASS |
| ADC-ELEM-TABLE-001 | PASS |
| ADC-ELEM-TABLE-002 | PASS |
| ADC-ELEM-TEXT-001 | PASS |
| ADC-ELEM-TEXT-002 | PASS |
| ADC-ELEM-TEXT-003 | PASS |
| ADC-ELEM-TONE-001 | PASS |
| ADC-PERF-001 | PASS |
| ADC-PERF-002 | PASS |
| ADC-SEC-001 | PASS |
| ADC-SEC-002 | PASS |
| ADC-SEC-003 | PASS |
| ADC-SEC-004 | PASS |
| ADC-SEC-005 | PASS |
| ADC-SEC-006 | PASS |
| ADC-SEC-007 | PASS |
| ADC-SEC-008 | PASS |
| ADC-SEC-009 | PASS |
| ADC-SLOT-001 | PASS |
| ADC-SLOT-002 | PASS |
| ADC-SLOT-003 | PASS |
| ADC-SLOT-004 | PASS |
| ADC-SLOT-005 | PASS |
| ADC-SLOT-006 | PASS |
| ADC-SLOT-007 | PASS |
| ADC-SLOT-008 | PASS |
| ADC-SLOT-009 | PASS |
| ADC-SLOT-010 | PASS |
| ADC-SLOT-011 | PASS |
| ADC-SLOT-012 | PASS |
| ADC-SLOT-013 | PASS |
| ADC-SLOT-014 | PASS |
| ADC-SLOT-015 | PASS |
| ADC-SLOT-016 | PASS |
| ADC-SLOT-017 | PASS |
| ADC-SLOT-018 | PASS |
| ADC-SLOT-019 | PASS |
| ADC-SLOT-020 | PASS |
| ADC-SLOT-021 | PASS |
| ADC-SLOT-022 | PASS |
| ADC-SLOT-023 | PASS |
| CPP-EFF-001 | PASS |
| CPP-EFF-002 | PASS |
| CPP-EFF-003 | PASS |
| CPP-EFF-004 | PASS |
| CPP-EFF-005 | PASS |
| CPP-EFF-006 | PASS |
| CPP-EFF-007 | PASS |
| CPP-EFF-008 | PASS |
| CPP-EFF-009 | PASS |
| CPP-EFF-010 | PASS |
| CPP-FIBER-001 | PASS |
| CPP-FIBER-002 | PASS |
| CPP-FIBER-003 | PASS |
| CPP-FIBER-004 | PASS |
| CPP-FIBER-005 | PASS |
| CPP-FIBER-006 | PASS |
| CPP-FIBER-007 | PASS |
| CPP-FIBER-008 | PASS |
| CPP-FIBER-009 | PASS |
| CPP-FIBER-010 | PASS |
| CPP-FIBER-011 | PASS |
| CPP-FIBER-012 | PASS |
| CPP-FIBER-013 | PASS |
| CPP-FIBER-014 | PASS |
| CPP-FIBER-015 | PASS |
| CPP-FIBER-016 | PASS |
| CPP-FIBER-017 | PASS |
| CPP-FIBER-018 | PASS |
| CPP-FIBER-019 | PASS |
| CPP-FIBER-020 | PASS |
| CPP-FIBER-021 | PASS |
| CPP-FIBER-022 | PASS |
| CPP-FIBER-023 | PASS |
| CPP-FIBER-024 | PASS |
| CPP-FIBER-025 | PASS |
| CPP-FIBER-026 | PASS |
| CPP-FIBER-027 | PASS |
| CPP-FIBER-101 | PASS |
| CPP-FIBER-102 | PASS |
| CPP-FIBER-103 | PASS |
| CPP-FIBER-104 | PASS |
| CPP-FIBER-105 | PASS |
| CPP-FIBER-111 | PASS |
| CPP-FIBER-112 | PASS |
| CPP-FIBER-113 | PASS |
| CPP-FIBER-114 | PASS |
| CPP-FIBER-115 | PASS |
| CPP-FIBER-116 | PASS |
| CPP-FIBER-121 | PASS |
| CPP-FIBER-122 | PASS |
| CPP-FIBER-123 | PASS |
| CPP-FIBER-124 | PASS |
| CPP-FIBER-125 | PASS |
| CPP-FIBER-126 | PASS |
| CPP-FIBER-127 | PASS |
| CPP-FIBER-131 | PASS |
| CPP-FIBER-132 | PASS |
| CPP-FIBER-133 | PASS |
| CPP-ISO-001 | PASS |
| CPP-ISO-002 | PASS |
| CPP-ISO-003 | PASS |
| CPP-ISO-004 | PASS |
| CPP-ISO-005 | PASS |
| CPP-ISO-006 | PASS |
| CPP-ISO-007 | PASS |
| CPP-ISO-008 | PASS |
| CPP-ISO-009 | PASS |
| CPP-ISO-010 | PASS |
| CPP-ISO-011 | PASS |
| CPP-ISO-012 | PASS |
| CPP-ISO-013 | PASS |
| CPP-ISO-014 | PASS |
| CPP-ISO-015 | PASS |
| CPP-ISO-018 | PASS |
| CPP-ISO-019 | PASS |
| CPP-ISO-020 | PASS |
| CPP-ISO-021 | PASS |
| CPP-ISO-026 | PASS |
| CPP-ISO-027 | PASS |
| CPP-ISO-028 | PASS |
| CPP-ISO-029 | PASS |
| CPP-ISO-030 | PASS |
| CPP-ISO-031 | PASS |
| CPP-MANIFEST-001 | PASS |
| CPP-MANIFEST-002 | PASS |
| CPP-MANIFEST-003 | PASS |
| CPP-MANIFEST-004 | PASS |
| CPP-MANIFEST-005 | PASS |
| CPP-MANIFEST-006 | PASS |
| CPP-MANIFEST-007 | PASS |
| CPP-MANIFEST-008 | PASS |
| CPP-MANIFEST-009 | PASS |
| CPP-MANIFEST-010 | PASS |
| CPP-MANIFEST-011 | PASS |
| CPP-MANIFEST-012 | PASS |
| CPP-MANIFEST-013 | PASS |
| CPP-MANIFEST-014 | PASS |
| CPP-MANIFEST-015 | PASS |
| CPP-MANIFEST-016 | PASS |
| CPP-MANIFEST-017 | PASS |
| CPP-MANIFEST-018 | PASS |
| CPP-MANIFEST-019 | PASS |
| CPP-MANIFEST-020 | PASS |
| CPP-MANIFEST-021 | PASS |
| CPP-MANIFEST-022 | PASS |
| CPP-MANIFEST-023 | PASS |
| CPP-MANIFEST-024 | PASS |
| CPP-MANIFEST-025 | PASS |
| CPP-MANIFEST-026 | PASS |
| CPP-PANEL-002 | PASS |
| CPP-PROC-001 | PASS |
| CPP-PROC-002 | PASS |
| CPP-PROC-003 | PASS |
| CPP-PROC-004 | PASS |
| CPP-PROC-005 | PASS |
| CPP-PROC-006 | PASS |
| CPP-PROC-007 | PASS |
| CPP-PROC-008 | PASS |
| CPP-PROC-009 | PASS |
| CPP-PROC-010 | PASS |
| CPP-PROC-020 | PASS |
| CPP-PROC-021 | PASS |
| CPP-PROC-022 | PASS |
| CPP-PROC-023 | PASS |
| CPP-PROC-024 | PASS |
| CPP-PROC-025 | PASS |
| CPP-PROC-026 | PASS |
| CPP-PROC-027 | PASS |
| CPP-PROC-028 | PASS |
| CPP-PROC-029 | PASS |
| CPP-PROC-030 | PASS |
| CPP-PROC-031 | PASS |
| CPP-PROC-032 | PASS |
| CPP-PROC-033 | PASS |
| CPP-SVC-001 | PASS |
| CPP-SVC-002 | PASS |
| CPP-SVC-003 | PASS |
| CPP-SVC-004 | PASS |
| CPP-SVC-005 | PASS |
| CPP-SVC-006 | PASS |
| CPP-SVC-007 | PASS |
| CPP-SVC-008 | PASS |
| CPP-SVC-009 | PASS |
| CPP-SVC-010 | PASS |
| CPP-SVC-011 | PASS |
| CPP-SVC-012 | PASS |
| CPP-SVC-013 | PASS |
| CPP-SVC-014 | PASS |
| CPP-TRUST-001 | PASS |
| CPP-TRUST-002 | PASS |
| CPP-TRUST-003 | PASS |
| CPP-TRUST-004 | PASS |
| CPP-TRUST-005 | PASS |
| CPP-TRUST-006 | PASS |
| CPP-TRUST-007 | PASS |
| CPP-TRUST-008 | PASS |
| CPP-TRUST-009 | PASS |
| CPP-TRUST-010 | PASS |
| CPP-TRUST-011 | PASS |
| CPP-TRUST-012 | PASS |
| CPP-TRUST-013 | PASS |
| CPP-TRUST-014 | PASS |
| CPP-TRUST-015 | PASS |
| CPP-TRUST-016 | PASS |
| CPP-TRUST-017 | PASS |
| CPP-TRUST-018 | PASS |
| CPP-VER-001 | PASS |
| CPP-VER-002 | PASS |
| CPP-VER-003 | PASS |
| CPP-VER-004 | PASS |
| CPP-VER-005 | PASS |
| CPP-VER-006 | PASS |
| CPP-VER-007 | PASS |
| CPP-WASM-001 | PASS |
| CPP-WASM-002 | PASS |
| CPP-WASM-003 | PASS |
| CPP-WASM-004 | PASS |
| CPP-WASM-005 | PASS |
| CPP-WASM-006 | PASS |
| CPP-WASM-007 | PASS |
| CPP-WASM-008 | PASS |
| CPP-WASM-009 | PASS |
| CPP-WASM-010 | PASS |
| CPP-WASM-011 | PASS |
| CPP-WASM-012 | PASS |
| CPP-WASM-013 | PASS |
| CPP-WASM-014 | PASS |
| CPP-WASM-015 | PASS |
| CPP-WASM-016 | PASS |
| CPP-WASM-017 | PASS |
| CPP-WASM-018 | PASS |
| CPP-WASM-019 | PASS |
| CPP-WASM-020 | PASS |
| CPP-WASM-021 | PASS |
| CPP-WASM-022 | PASS |
| CPP-WASM-023 | PASS |
| CPP-WASM-024 | PASS |
| CPP-WASM-025 | PASS |
| CPP-WASM-026 | PASS |
| CPP-WASM-027 | PASS |
| CPP-WASM-033 | PASS |
| RPC-L2-001 | PASS |
| RPC-L2-002 | PASS |
| RPC-L2-003 | PASS |
| RPC-L2-004 | PASS |
| RPC-L2-005 | PASS |
| RPC-L2-006 | PASS |
| RPC-L2-007 | PASS |
| RPC-L2-008 | PASS |
| RPC-L2-009 | PASS |
| RPC-L2-010 | PASS |
| RPC-L2-101 | PASS |
| RPC-L2-102 | PASS |
| RPC-L2-103 | PASS |
| RPC-L2-104 | PASS |
| RPC-L2-105 | PASS |
| RPC-L2-106 | PASS |
| RPC-L2-107 | PASS |
| RPC-L2-108 | PASS |
| RPC-L2-109 | PASS |
| RPC-L2-110 | PASS |
| RPC-L2-111 | PASS |
| RPC-L2-112 | PASS |
| RPC-L2-113 | PASS |
| RPC-L2-114 | PASS |
| RPC-L2-115 | PASS |
| RPC-L2-116 | PASS |
| RPC-L2-117 | PASS |
| RPC-L2-118 | PASS |
| RPC-L2-119 | PASS |
| RPC-L2-121 | PASS |
| RPC-L2-122 | PASS |
| RPC-L2-123 | PASS |
| RPC-L2-124 | PASS |
| RPC-L2-125 | PASS |
| RPC-L2-126 | PASS |
| RPC-L2-127 | PASS |
| RPC-L2-128 | PASS |
| RPC-L2-202 | PASS |
| RPC-L2-203 | PASS |
| RPC-L2-204 | PASS |
| RPC-L2-205 | PASS |
| RPC-L2-206 | PASS |
| RPC-L2-207 | PASS |
| RPC-L2-208 | PASS |
| RPC-L2-221 | PASS |
| RPC-L2-222 | PASS |
| RPC-L2-223 | PASS |
| RPC-L2-224 | PASS |
| RPC-L2-225 | PASS |
| RPC-L2-226 | PASS |
| RPC-L2-227 | PASS |
| RPC-L2-228 | PASS |
| RPC-L2-301 | PASS |
| RPC-L2-302 | PASS |
| RPC-L2-303 | PASS |
| RPC-L2-304 | PASS |
| RPC-L2-305 | PASS |
| RPC-L2-306 | PASS |
| RPC-L2-307 | PASS |
| RPC-L2-308 | PASS |
| RPC-L2-309 | PASS |
| RPC-L2-310 | PASS |
| RPC-L2-311 | PASS |
| RPC-L2-312 | PASS |
| RPC-L2-313 | PASS |
| RPC-L2-401 | PASS |
| RPC-L2-402 | PASS |
| RPC-L2-403 | PASS |
| RPC-L2-404 | PASS |
| RPC-L2-405 | PASS |
| RPC-L2-406 | PASS |
| RPC-L2-407 | PASS |
| RPC-L2-408 | PASS |
| RPC-L2-409 | PASS |
| RPC-L2-410 | PASS |
| MCP-CLIENT-001 | PASS |
| MCP-CLIENT-002 | PASS |
| MCP-CLIENT-003 | PASS |
| MCP-CLIENT-004 | PASS |
| MCP-CLIENT-005 | PASS |
| MCP-CLIENT-006 | PASS |
| MCP-CLIENT-007 | PASS |
| MCP-CLIENT-008 | PASS |
| MCP-CLIENT-009 | PASS |
| MCP-CLIENT-010 | PASS |
| MCP-CLIENT-011 | PASS |
| MCP-CLIENT-012 | PASS |
| MCP-CLIENT-013 | PASS |
| MCP-CLIENT-014 | PASS |
| MCP-CLIENT-015 | PASS |
| MCP-CLIENT-016 | PASS |
| MCP-CLIENT-017 | PASS |
| JS-L2SVC-001 | PASS |
| JS-L2SVC-002 | PASS |
| JS-L2SVC-003 | PASS |
| JS-L2SVC-004 | PASS |
| JS-L2SVC-005 | PASS |
| JS-L2SVC-006 | PASS |
| JS-L2SVC-007 | PASS |
| JS-L2SVC-008 | PASS |
| JS-L2SVC-009 | PASS |
| JS-L2SVC-010 | PASS |
| JS-L2SVC-011 | PASS |
| JS-L2SVC-012 | PASS |
| JS-L2SVC-013 | PASS |
| JS-L2SVC-021 | PASS |
| JS-L2SVC-022 | PASS |
| JS-L2SVC-023 | PASS |
| JS-L2SVC-024 | PASS |
| JS-L2SVC-025 | PASS |
| JS-L2SVC-026 | PASS |
| JS-L2SVC-027 | PASS |
| JS-L2SVC-028 | PASS |
| M3-DEFAULT-001 | PASS |
| M3-DEFAULT-002 | PASS |
| M3-DEFAULT-003 | PASS |
| M3-DEFAULT-004 | PASS |
| M3-DEFAULT-010 | PASS |
| M3-DEFAULT-011 | PASS |
| M3-DEFAULT-012 | PASS |
| M3-DEFAULT-013 | PASS |
| M3-DEFAULT-014 | PASS |
| M3-DEFAULT-015 | PASS |
| M3-DEFAULT-020 | PASS |
| M3-DEFAULT-021 | PASS |
| M3-DEFAULT-022 | PASS |
| M3-DEFAULT-023 | PASS |
| M3-DEFAULT-024 | PASS |
| M3-DEFAULT-025 | PASS |
| M3-DEFAULT-030 | PASS |
| M3-DEFAULT-031 | PASS |
| M3-DEFAULT-032 | PASS |
| M3-DEFAULT-035 | PASS |
| M3-DEFAULT-036 | PASS |
| M3-DEFAULT-037 | PASS |
| M3-DEFAULT-040 | PASS |
| M3-DEFAULT-041 | PASS |
| M3-DEFAULT-042 | PASS |
| M3-DEFAULT-043 | PASS |
| M3-DEFAULT-044 | PASS |
| M3-DEFAULT-045 | PASS |
| M3-DEFAULT-050 | PASS |
| M3-DEFAULT-051 | PASS |
| M3-DEFAULT-052 | PASS |
| M3-DEFAULT-053 | PASS |
| M3-DEFAULT-054 | PASS |
| M3-DEFAULT-055 | PASS |
| M3-DEFAULT-056 | PASS |
| M3-DEFAULT-060 | PASS |
| M3-DEFAULT-061 | PASS |
| M3-DEFAULT-062 | PASS |
| M3-DEFAULT-063 | PASS |
| M3-DIGEST-001 | PASS |
| M3-DIGEST-002 | PASS |
| M3-INSTALL-001 | PASS |
| M3-INSTALL-002 | PASS |
| M3-INSTALL-003 | PASS |
| M3-LOADER-001 | PASS |
| M3-LOADER-002 | PASS |
| M3-LOADER-003 | PASS |
| M3-LOADER-004 | PASS |
| M3-LOADER-005 | PASS |
| M3-LOADER-006 | PASS |
| M3-LOADER-007 | PASS |
| M3-LOADER-008 | PASS |
| M3-LOADER-009 | PASS |
| M3-LOADER-010 | PASS |
| M3-LOADER-011 | PASS |
| M3-LOADER-012 | PASS |
| M3-LOADER-013 | PASS |
| M3-LOADER-014 | PASS |
| M3-LOADER-015 | PASS |
| M3-PARALLEL-001 | PASS |
| M3-PARALLEL-002 | PASS |
| M3-PARALLEL-003 | PASS |
| M3-PARALLEL-004 | PASS |
| M3-PARALLEL-005 | PASS |
| M3-PARALLEL-006 | PASS |
| M3-PARALLEL-007 | PASS |
| M3-PARALLEL-101 | PASS |
| M3-PARALLEL-102 | PASS |
| M3-PARALLEL-103 | PASS |
| M3-PARALLEL-104 | PASS |
| M3-PARALLEL-105 | PASS |
| M3-PARALLEL-106 | PASS |
| M3-PARALLEL-107 | PASS |
| M3-PARALLEL-108 | PASS |
| M3-PARALLEL-109 | PASS |
| M3-PARALLEL-110 | PASS |
| M4-A11Y-001 | PASS |
| M4-ADC-001 | PASS |
| M4-ADC-002 | PASS |
| M4-ADC-003 | PASS |
| M4-ADC-004 | PASS |
| M4-ADC-005 | PASS |
| M4-ADC-006 | PASS |
| M4-ADC-007 | PASS |
| M4-ADC-008 | PASS |
| M4-ADC-009 | PASS |
| M4-ADC-010 | PASS |
| M4-ADC-011 | PASS |
| M4-ADC-012 | PASS |
| M4-ADC-013 | PASS |
| M4-ADC-014 | PASS |
| M4-ADC-015 | PASS |
| M4-ADC-016 | PASS |
| M4-ADC-017 | PASS |
| M4-ADC-018 | PASS |
| M4-ADC-019 | PASS |
| M4-ADC-020 | PASS |
| M4-ADC-021 | PASS |
| M4-ADC-022 | PASS |
| M4-ADC-023 | PASS |
| M4-COMPONENT-001 | PASS |
| M4-COMPONENT-002 | PASS |
| M4-COMPONENT-003 | PASS |
| M4-COMPONENT-004 | PASS |
| M4-COMPONENT-005 | PASS |
| M4-COMPONENT-006 | PASS |
| M4-COMPONENT-007 | PASS |
| M4-CONC-001 | PASS |
| M4-CONC-002 | PASS |
| M4-CONC-003 | PASS |
| M4-KEY-001 | PASS |
| M4-KEY-002 | PASS |
| M4-KEY-003 | PASS |
| M4-KEY-004 | PASS |
| M4-KEY-005 | PASS |
| M4-KEY-006 | PASS |
| M4-KEY-007 | PASS |
| M4-KEY-008 | PASS |
| M4-KEY-009 | PASS |
| M4-KEY-010 | PASS |
| M4-TRAIT-001 | PASS |
| M4-UNINSTALL-001 | PASS |
| M4-UNINSTALL-002 | PASS |
| M4-UNINSTALL-003 | PASS |
| M4-UNINSTALL-004 | PASS |
| M4-UNINSTALL-005 | PASS |
| M4-UNINSTALL-006 | PASS |
| M4-UNINSTALL-007 | PASS |
| M4-UNINSTALL-008 | PASS |

## 说明与已知缺口

- groups 批量条目的 source_dirs 由 harness 扫描：测试函数名内编码或 // ID / /// ID 注释匹配，找不到对应测试函数的 ID 会直接报错，防止把未实现/无测试的 ID 标成 implemented。
- 本地 --full 实跑（2026-11-15，本机受限会话）：67 suites / 836 passed / 1 failed / 6 ignored；失败包仅 dshr-sidecar-guard（process::tests::drop_terminates_the_guarded_process，沙箱进程限制，隔离复跑同样失败，非代码）。普通终端/Lead 环境同 workspace 0 failed / 6 ignored。默认 `cargo test --workspace`（无 --no-fail-fast）在本机会先止于 sidecar-guard、到不了 official_packages；--no-fail-fast 输出里 official_packages 为 0 passed / 6 ignored。
- P1-6 ignored 检测：cargo suite 按 `Running ...(<binary>-<hash>.exe)` + 紧随 `test result:` 建 binary→counts 映射（stdout/stderr 合并同一文件以保顺序）；integration test 二进制用 `crates/<pkg>/tests/<binary>.rs` 反查归属。`passed=0 && ignored>0` 的整文件忽略二进制会列入 suite summary（实测：`未执行: official_packages(6 ignored)`），该包若无其它已执行用例则判 FAIL；所有依赖 official fixture 的 ID 已改 manual，不会把 ignored 算 pass。
- W8/P0 证据：CPP-WASM-021..027、CPP-ISO-018/019/026..031、M3-INSTALL-001..003、M3-DIGEST-001/002、CPP-PANEL-002、CPP-WASM-033、RPC-L2-202..208、L2JS-021..028、M3-PARALLEL-108..110 为 cargo/node-test implemented；CPP-PROC-024..027 由既有 plugin-sdk 组覆盖（源码 session.rs/tool_result.rs 有同 ID 重复，注册表按唯一 ID 登记）；RPC-L2-201/501..507 与 chat --selftest-wasm（CPP-WASM-100）为 manual。
- ID 兼容说明：MCP 源码两段式 MCP-001..017 / JS L2JS-001..028 按 spec/README §3 三段式登记为 MCP-CLIENT-001..017 / JS-L2SVC-001..028，title/notes 保留源码 ID。
- manual 证据：official gate（RPC-L2-201/501..507）= `powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1`（缺 .tmp/dsh-tools-all 时 exit 1；默认 cargo 8 ignored/0 passed）；chat selftests：CPP-TRUST-019/CPP-ISO-022/RPC-L2-411/ADC-PANEL-001/M3-DEFAULT-070/M3-PARALLEL-201/CPP-WASM-100/M4-LIFECYCLE-001（需构建 chat 二进制，部分需 vendored sidecar）；M4 CLI：M4-UNINSTALL-100（--uninstall-plugin）、M4-KEY-100（--trust-key）、M4-SDK-001/M4-SIGN-001（tools/authoring-check.ps1 的 SDK 离线包与签名/轮换步骤）、M4-KEYBOARD-001（真实窗口无鼠标流程，见 docs/m4-keyboard.md）、M4-ADC-100（插件面板 Button copy/call 真实点击）、M4-ADC-101（openExternal 确认条）、M4-P2-001（node 缺失时测试失败而非 SKIP）、M4-RECOVERY-001/002/003（chat --selftest-recovery、--selftest-sidecar-recovery、--selftest-crash-recovery）、M4-PERF-001（node tools/perf-gate.mjs）、M4-CI-001（cross-platform.yml）、M4-CI-002（node tools/official-provenance.mjs --check）、M4-CI-003（WSL Linux 核心/桌面/自检）、M4-CI-004（package-desktop.ps1/sh）、M4-CI-005（verify-package.ps1/sh）、M4-OFFICIAL-001（chat --selftest-official）、M4-DRAW-001（tools/frame-probe.ps1）、RPC-L2-506/507（official fs read/write）；CPP-AUTHOR-001 = tools/authoring-check.ps1；GATE-G2-001 = 仓库外实验 checklist。
- 提交矩阵由 `--full --compact` 生成（cargo 结果真实、不是 SKIP）；quick 门禁仍用 `--quick`。cargo suite 会把 #[ignore] 计入 ignored、绝不当作 passed：包级 selector 遇 ignored>0 判 FAIL，all 通过时也在消息中标注 ignored 数。compact markdown 的 PASS 行只列 ID+结果，非 PASS（FAIL/SKIP/PENDING/MANUAL）保留完整 6 列；JSON tests.rows 为 [id, area, result]。
- .github/workflows/conformance.yml 未在 GitHub Actions 实跑，矩阵由本机命令生成。
- M4 W1（2026-11-16）：`M4-UNINSTALL-001..008`（crates/profile/src/uninstall.rs + lib.rs）与 `M4-KEY-001..010`（crates/profile/src/trust.rs）为 cargo implemented（selector=dshr-profile）；`M4-UNINSTALL-100`/`M4-KEY-100`/`M4-LIFECYCLE-001`/`M4-SDK-001`/`M4-SIGN-001` 是 chat/工具链产品路径 manual 证据。三平台 CI 与 macOS/Linux 行为仍未验证（[m4-status.md](m4-status.md) §3）。
- M4 W3（2026-11-16）：`M4-ADC-001`（ui-gpui handler 穿透）、`M4-ADC-002`（PluginHost 路由）、`M4-ADC-003`（external-process ui.action→render）、`M4-ADC-004`（tool.result.meta.ui→ToolCard.extra）、`M4-ADC-005`（Checkbox/Switch onChange 渲染）、`M4-ADC-006`（Select onChange 注入 value）为 cargo implemented；`CPP-PROC-033` 覆盖 SDK ActionHandler；`M4-ADC-100/101` 为真实点击/确认条 manual。Input/Textarea/Slider onChange 与 draw 预算仍未接。
- M4 W8（2026-11-16）：`M4-P2-001` 记录 SKIP-as-pass 修复——`crates/compat-dsh-host/tests/{parallel_batch,l2_host_services}.rs` 在 node 缺失时 panic/exit 101（实测限 PATH 后），不再 eprintln SKIP+return 记 passed；普通环境仍全绿。
- M4 G2（2026-11-16）：`M4-RECOVERY-001` = `chat --selftest-recovery`，覆盖损坏会话隔离+重建、删除主文件后 `.bak` 恢复并继续追加、失败插件回滚+健康插件装载/调用；`M4-RECOVERY-002` = `--selftest-sidecar-recovery`，kill sidecar 后会话仍可追加、原生工具仍注册、已死调用返回错误；`M4-RECOVERY-003` = `--selftest-crash-recovery`，子进程删主文件并 abort 后父进程从 `.bak` 恢复并继续追加。headless PASS，真实窗口崩溃恢复未覆盖。
- M4 W6（2026-11-16）：`RPC-L2-228` 覆盖 `session-log/v1 events.append`（projections 仍只读）；sidecar 最小 `exec.agent.session.append` → 该写通道。`official-l2-gate.ps1` 8/8 PASS：官方 `dsh-tool-todo` execute 写入 `todo/write`，官方 `working_directory` execute 真实返回 cwd，官方 `dsh-tool-fs` read/write execute 真实读取/落盘；bash/pwsh 等 shell 工具仍未闭环。
- M4 G4（2026-11-16）：`M4-PERF-001` = `node tools/perf-gate.mjs`（阈值 50ms / 满配额 16.7ms / a11y 0），CI conformance.yml 已加 step 与 perf-baseline artifact；本机 PASS（worst 10.799ms、max_quota 4.033ms），Actions 未实跑。
- M4 W4（2026-11-16）：`M4-TRAIT-001` 覆盖 `Plugin::id/version` 返回借用 `&str`，external/WASM 插件删除 `Box::leak`；native/MCP 并发与 `Plugin::call` 异步/取消仍未做。
- M4 W7/G1（2026-11-16）：新增 `.github/workflows/cross-platform.yml`（三平台 contract + 19 个核心 crate 测试，含 external-process/WASM）；YAML 已本地解析，Actions 未实跑；桌面 GPUI 仍 Windows-only，G1 不标完成。
- M4 W7 官方包来源（2026-11-16）：`M4-CI-002` = `node tools/official-provenance.mjs --check`，校验 3 个官方包版本 + package.json/lib/index.js SHA-256；`official-l2-gate.ps1` 在测试前强制执行，gate 8/8 PASS。
- M4 G1 Linux（2026-11-17）：`M4-CI-003` = WSL Ubuntu 22.04 + Rust 1.99.1 对核心 6 crate `cargo check` 通过；修复 `sidecar-guard` 非 Windows spawn 缺 `mut command`；仅核心 crate，不含 GPUI/桌面。
- compact 矩阵：markdown 保留逐 ID 的 ID/领域/状态/结果/证据/消息；JSON 的 tests.rows 为 [id, area, result]（status/证据/消息见 markdown 与注册表，result=pending|manual 即对应 status）。
- quick 模式不运行 cargo suite：implemented 且 suite=cargo 的 ID 显示 SKIP 属正常，需 --workspace 或 --full。
- cargo suite 在 Windows 上经 tools/build-env.cmd 调用（cargo test --offline --workspace --no-fail-fast）。
- .github/workflows/conformance.yml 未在 GitHub Actions 实跑，本报告由本机命令生成。
- 本文件由 tools/conformance.mjs 生成，请勿手改；新增证据请改 docs/conformance.d/registry.json。

退出码：`0`。pending / manual / blocked 不计失败。

