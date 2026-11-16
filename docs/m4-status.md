# M4 状态 — 跨平台 Beta

> 更新：2026-11-16（W1 完成：离线渠道口径；uninstall/密钥生命周期/签名安装/SDK 离线包）。
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
| W2 | 键盘与无障碍 | 未开始 | 需盘点现有快捷键/焦点实现 |
| W3 | ADC 交互与工具卡 | 未开始 | M3 只到 `AnyElement` 面板，事件未回 Host |
| W4 | native/MCP 并发 + trait 冻结 | 未开始 | `Plugin::call` 仍同步、`&'static str` leak |
| W5 | WASM Component/WIT | 未开始 | 当前 `dshr-wasm-core/v1` 是 core module |
| W6 | 官方 execute 闭环 | 未开始 | `exec.agent` 债务 |
| W7 | 三平台 CI / 性能基线 | 未开始 | 本机 Windows only；Actions 未实跑 |
| W8 | P2 余项 | 未开始 | review P2 表 |

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

powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1 -SkipBuild
  → [authoring-check] PASS（含卸载生命周期；exit 0）

powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1 -SkipBuild
  → PASS（SDK 离线包/生成/构建/安装/卸载/签名安装/密钥轮换；exit 0）

node schema\check.mjs
  → 76/76 cases passed

node tools\conformance.mjs --full --compact
  → 67 suites / 855 passed / 0 failed / 6 ignored；exit 0
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

## 4. Gate 对账

| Gate | 判定 | 当前状态 | 缺口 |
|---|---|---|---|
| G1 三平台 + 两类插件 | 三平台 CI 全绿；两类来源插件回归 | **未验证** | 无 macOS/Linux runner；DSH JS 插件跨平台未跑 |
| G2 崩溃/插件失败恢复 | 数据可恢复、可继续操作 | 未开始 | 需故障注入与恢复断言 |
| G3 键盘全流程 | 无鼠标完成主流程 | 未开始 | 需焦点/快捷键审计与自动断言 |
| G4 性能基线进 CI | 基线 + 阈值由 CI 执行 | 未开始 | 现有脚本是手动；需 W7 |

## 5. 下一步（按依赖排序）

1. **W2**：盘点键盘路径，先做"新建→发送→审批→切面板→关闭"的自动断言与焦点顺序。
2. **W3**：ADC 交互事件回到 Host；tool card `tool.result.extra`。
3. **W7**：三平台 CI matrix、官方包 vendor 来源校验、性能基线进 CI（G1/G4 的前置）。

## 6. 复现命令

```powershell
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
tools\build-env.cmd cargo test --offline -p dshr-profile
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-plugin-lifecycle
powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1 -SkipBuild
```
