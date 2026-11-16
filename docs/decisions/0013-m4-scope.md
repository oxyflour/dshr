# ADR-0013 — M4 范围锁定、工作包顺序与 Beta gate 口径

- 状态：Accepted
- 日期：2026-11-16
- 关联：[`../PLAN.md`](../PLAN.md) §8 M4 / §9.2、[`0012-m3-gate-closure.md`](0012-m3-gate-closure.md)、[`../m3-status.md`](../m3-status.md)、[`../m4-status.md`](../m4-status.md)

## 背景

M3 已按 [ADR-0012](0012-m3-gate-closure.md) 的口径完成：G1 契约冻结、G2 降级为
"内部作者 Preview"、G3 以 core-module WASM + manual official gate 达标。M3 收尾同时留下
PLAN §9.2 的 8 项追加候选（80–140 工程日）与 M4 基线的跨平台/无障碍/性能/安全项。

2026-11-16 产品确认：**时间无硬指标、不设硬上限**；预算仍是估算，不作为砍单依据。
因此 M4 不按日历切，而按"可判定工作包"推进：每个包必须有测试 ID、可复现命令与
文档更新，未验证项不得写成已达标。

当前环境事实（决定本轮能做什么）：

- 本机是 Windows 11 / Rust stable；GPUI 固定在 zed `d2a4fb6`，macOS/Linux 构建与
  WebView2 之外的窗口系统**无法在本机验证**。
- 官方 DSH 插件 fixture 只能从本机安装包抽取（`tools/dsh-vendor.mjs`），不能提交；
  GitHub Actions 尚未实跑。
- 因此 M4 的四条 PLAN gate 中，G1（三平台）与 G4（性能进 CI）需要 CI runner 证据，
  G2/G3 可以本机自动化 + 自检覆盖。

## 选项

| 选项 | 内容 | 结论 |
|---|---|---|
| A. 按 PLAN M4 七条平铺 | 跨平台、多窗口/a11y、WebView、ACP/MCP、安全、性能、文档同时开工 | 否决：没有可判定的提交边界，跨平台项在本机不可验证，容易再次出现"文档声称 > 实际" |
| B. 只做 §9.2 追加候选 | 把 8 项债务清完即称 Beta | 否决：漏掉 PLAN 基线的键盘全流程、崩溃恢复、性能基线进 CI，Beta gate 不成立 |
| **C. 工作包 + 四条 gate 对账** | W1–W8 逐包交付；每个包绑定 gate 与测试 ID；本机可验的先做，CI/三平台项显式标"未验证" | **采用** |

## 决定

### 1. 工作包与顺序

顺序按依赖与产品风险排列；每包完成即更新 [`../m4-status.md`](../m4-status.md)，不一次宣称整个 M4 完成。

| # | 工作包 | 交付物 | 主要测试 ID | 对应 gate |
|---|---|---|---|---|
| **W1** | **分发与信任生命周期** | `--uninstall-plugin`（目录 + profile + 审计原子化）；密钥吊销/过期/轮换与密钥审计；作者签名打包；SDK 离线渠道（manifest + 校验和） | `M4-UNINSTALL-*` / `M4-KEY-*` / `M4-SDK-001` / `M4-SIGN-001` | G2 |
| **W2** | **键盘与无障碍** | 焦点顺序、控件 label、键盘全流程（新建→发送→审批→切面板→关闭）；a11y 回归测试 | `M4-A11Y-*` | G3 |
| **W3** | **ADC 交互与工具卡** | Button/Input/Select 等事件回到 Host；tool card 内 ADC `tool.result.extra`；窗口 draw 预算 | `M4-ADC-*` | G1/G4 |
| **W4** | **并发与 Plugin trait 冻结** | native/MCP 工具并发；`Plugin::call` 取消/异步/泄漏收口；trait 冻结 ADR | `M4-CONC-*` | G2 |
| **W5** | **WASM Component/WIT** | canonical ABI、工具/面板声明读取、与 core-module 路径共存/迁移 | `M4-WIT-*` | G2 |
| **W6** | **官方 execute 闭环** | `exec.agent` 债务的 sidecar 侧适配；官方工具 execute 真实往返 | `M4-OFFICIAL-*` | G2 |
| **W7** | **三平台 CI 与性能基线** | Windows/macOS/Linux matrix；官方包 vendor/来源校验；性能基线进 CI | `M4-CI-*` / `M4-PERF-*` | G1/G4 |
| **W8** | **P2 余项** | 矩阵元数据、事件顺序文档、`session_id` 接线、flaky 治理 | `M4-P2-*` | — |

**W1 完成口径**（2026-11-16）：uninstall、密钥生命周期、作者签名打包/安装、SDK 离线
发布包与消费链路已交付；**registry、CA/证书链、自动更新与吊销分发不在 W1**，归入 W7。

### 2. 四条 gate 的口径（沿用 PLAN §8 M4，不改判定）

| Gate | 判定 | 当前可验证性 |
|---|---|---|
| **G1 三平台 + 两类插件** | Windows/macOS/Linux CI 全绿；Rust Native 与 DSH JS 两类来源插件跨平台回归通过 | 本机只能 Windows；macOS/Linux 必须等 W7 runner，未跑前标 **未验证**，不得写成达标 |
| **G2 崩溃/插件失败恢复** | 崩溃或插件失败后数据可恢复、用户可继续操作 | 本机可做故障注入（杀 sidecar、坏插件、卸载/重装） |
| **G3 键盘全流程** | 不需鼠标完成新建→发送→审批→切面板→关闭 | 本机可做自动断言 + 人工复验；自动断言优先 |
| **G4 性能基线进 CI** | 性能基线与回归阈值由 CI 执行 | 依赖 W7；本机先固定基线脚本与阈值 |

### 3. 与 M5 的边界

M4 不承诺：完整 DSH React Client、组件市场、团队任务板、云端/多用户、3D/EDA 视口渲染器、
任意 DLL 热卸载、DSH L5。以上仍按 PLAN §9.1「不含项」处理。

### 4. 预算与砍单

- 预算：PLAN 基线 50–100 + §9.2 追加 80–140；时间无硬指标，不按上限砍 G1–G4 判定项。
- 若外部依赖阻塞（macOS runner 不可得、官方 fixture 无法合法 vendor），该 gate 标
  `blocked-on-environment` 并把可本机验证的部分做完，**不得**用文档或手动步骤冒充通过。
- 每个工作包必须留下"未验证项"清单；只有四条 gate 都有证据时才能把 M4 标完成。

## 后果

**好消息**：M4 从"七条平铺 + 未知预算"变成 8 个可独立验收的包；W1/W2/W3 都能在本机
产出自动化证据；跨平台与 CI 项明确标为环境阻塞，避免重演 M3 P0 的"gate 虚假"。

**坏消息**：三平台 gate 在拿到 CI runner 之前无法完成；W4 的 trait 冻结会触及
`plugin-api`/`plugin-host`/wasm/external 多条链路，属于高风险重构；W5/W6 依赖外部
运行时事实，可能继续延期。

## 反悔条件

- macOS/Linux CI 长期不可得 → G1 改为"Windows 发布 + 另两平台编译检查/社区报告"，
  并同步下调 Beta 对外口径（需产品确认）。
- `Plugin::call` 异步化引发 ABI/生命周期不可控 → 保留同步 `call`，把取消/超时放到
  宿主边界，W4 只做并发与泄漏收口（写新 ADR）。
- 官方 `execute` 的 `exec.agent` 债务无法在 sidecar 侧闭合 → W6 降级为"装载 + 方法往返"
  并明确写进已知限制，不阻塞 G2。
- WASM Component canonical ABI 与现有线程/宿主模型冲突 → 保留 core-module 主路径，
  Component 作为实验能力，W5 标 partial。

## 验证

- 每包：`cargo test -p <crate>` 或 `chat --selftest-*` 的 PASS 输出；
- 文档与注册表：`node schema/check.mjs`、`node tools/check-links.mjs`、
  `node tools/conformance.mjs --quick`；
- 全量：`node tools/conformance.mjs --workspace`（Rust）与 `--full`（矩阵落盘）。
- M4 完成条件：四条 gate 的证据全部可复现，且 `m4-status.md` 不再有 `未验证` 的 gate 项。
