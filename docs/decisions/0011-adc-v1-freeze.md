# ADR-0011 — ADC-v1 冻结（M3 G1）

- 状态：Accepted
- 日期：2026-11-14
- 关联：[`../PLAN.md`](../PLAN.md) §8 M3 G1、[`adc-v1.md`](../spec/adc-v1.md)、
  [`slots.v1.json`](../../schema/adc-v1/slots.v1.json)、[`../m3-status.md`](../m3-status.md)

## 背景

M3 的 G1 要求"ADC-v1 元素集冻结，未知元素降级渲染的测试通过"。W1 完成了冻结工件
（spec Review、12 slot 目录、26 元素字段级 schema、§12 未决清零），W2 完成了服务端
（`ui-contributions` 注册表 + `parse_tree`）与渲染器（`ui-gpui::adc_view`）。现在把
`adc/v1` 从 `Review` 改为 `Frozen`，并明确冻结边界、变更政策与未实现项的定性。

## 决定

1. **冻结范围**（`adc/v1`）：
   - 12 个 slot 的 `slot_id` / `kind` / `scope` / 注册契约 / key 域（`schema/adc-v1/slots.v1.json` 为唯一事实源）；
   - 26 个元素类型与字段级 props（`schema/adc-v1/element.schema.json`）；
   - 绑定前缀（`session.`/`workspace.`/`tool.`/`plugin.`）、动作（`call`/`command`/`openExternal`/`copy`）、
     事件（`onPress`/`onChange`/`onSubmit`）、配额（8 层/200 节点/8 KB 文本/256 patch）；
   - 前向兼容语义：未知元素运行时降级为 `Placeholder` + 诊断、不 panic、不丢整棵树（P4）。
2. **不在 v1**：元素级 WebView/DOM/HTML、任意 CSS/脚本、工具卡内任意渲染、Video/Canvas；
   需要 DOM 的插件走独立 Panel（Host 单独授权，M4 后评估）。§12 Q5 的结论保持不变。
3. **变更政策**（spec README 的状态机）：
   - 新增元素/属性/新 slot = **minor**，插件用 `ctx.capability('adc.element.<Name>')` 探测；
   - 修改既有元素语义 = **major**，旧元素集至少保留一个 major 周期；
   - 删除元素 = **major**，先 `deprecated` 再删；
   - 破坏性变更必须走 spec 变更流程 + 迁移说明，**不再以"冻结前讨论"为理由跳版**。
4. **冻结不承诺渲染完整度**：渲染器允许对个别元素/子特性降级（见 §后果），但元素**语义**不变；
   降级必须在 `ui-gpui` 文档与 conformance 中写明，不得静默丢弃。
5. **a11y 的冻结口径**：交互元素运行时 MUST 有 `a11yLabel`（作者期 schema 可选、运行时拒绝）；
   焦点顺序=文档序；键盘可达 `Tab`/`Enter`/`Space`/`Esc`；完整 WCAG 审计留给 M4。

## 后果

**好消息**：第三方作者可以按 `adc/v1` 写插件，宿主承诺至少一个 major 周期的兼容；未知元素降级
有实现与测试（`ADC-ELEM-*`）；slot 目录与元素 schema 是机器可读的冻结事实源。

**坏消息 / 已知渲染降级（实现限制，不是规范）**：
- `Chart` 的 `line`/`donut` 当前用近似绘制（细柱/比例条），`bars` 完整；
- `Image` 只渲染占位框 + `alt` + 附件 id（M3 不取网络/附件内容）；
- 交互元素只做视觉渲染，事件/动作回传与焦点顺序尚未接线（`adc_view` 是纯渲染函数）；
- 渲染预算（典型树 draw p95）尚未实测，作为 M3 剩余验收项（不阻塞冻结语义）。

## 反悔条件

- 若渲染预算实测证明 26 元素在典型树下不可承受：按 ADR-0007 的砍单路径把交互元素延后，
  **以只读子集重新冻结**（先改 spec 再改实现），不得保留"假冻结"；
- 若外部作者反馈冻结范围不足：走 minor（新增元素）而不是破坏性修改；
- 若 W3C/WCAG 审计发现 a11y 契约错误：minor 修正（补强制项）可以接受，删除强制项须 major。

## 验证

- `node schema/check.mjs` → 71/71（含 slot 目录自检、26 元素 props 正反例、注册 kind 差异）；
- `cargo test -p dshr-ui-contributions` → `ADC-SLOT-001..023`、`ADC-ELEM-001..022`、`ADC-SEC-001..009`；
- `cargo test -p dshr-ui-gpui` → 26 个 ADC-ELEM 渲染器测试（含 26 元素全量无窗口 smoke）；
- conformance 注册表：ADC 相关 ID 均为 implemented；`ADC-BIND-001` 保持 pending（事件/绑定接线未做）。
