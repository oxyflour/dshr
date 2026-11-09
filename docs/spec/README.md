# 设计规范（docs/spec）

这里是**对外承诺的契约**。`docs/PLAN.md` 说明为什么做、什么时候做；本目录说明**做什么、边界在哪、怎么验证**。实现与规范冲突时，以规范为准；改行为必须先改规范。

## 1. 规范清单

| 文件 | 内容 | 状态 | 冻结时点 |
|---|---|---|---|
| [`adc-v1.md`](adc-v1.md) | Agent Desktop Client API v1：slot 注册表、元素词汇表、绑定与动作 | Draft | M3 |
| [`cpp-v1.md`](cpp-v1.md) | Cordis Portable Profile v1：Rust 插件 manifest、生命周期、Service/Effect/Tool | Draft | M3 |
| [`sidecar-rpc.md`](sidecar-rpc.md) | Rust ↔ Node DSH Sidecar 的跨进程协议 | Draft | M2 |
| [`session-log.md`](session-log.md) | Session 事件日志：信封、事件类型、回放与上下文事件 | Draft | M2 |

状态含义：

- **Draft**：可以改，可以删；实现方不得对外承诺兼容。
- **Review**：形状已定，只允许向后兼容的补充。
- **Frozen**：破坏性变更必须升 major，并提供至少一个 major 周期的迁移窗口。

## 2. 关键词

- **MUST / 必须**：不满足即不合规，conformance 测试会失败。
- **SHOULD / 应该**：允许偏离，但必须在实现里写明理由。
- **MAY / 可以**：可选能力，通过 capability 协商暴露。

## 3. 标识符规则

| 标识 | 形态 | 规则 |
|---|---|---|
| `plugin_id` | `vendor.name` | 小写 ASCII、`[a-z0-9]` 与分隔符 `.` `-` `_`；反向域名风格；全局唯一 |
| `slot_id` | `domain.seat` | 点分小写，如 `tool.call.toolview`、`settings.general.item`；由宿主声明 |
| `service_key` | `name/version` | 如 `tool-registry/v1`；版本是整数 major |
| `tool_name` | `[a-z][a-z0-9_]*` | 模型可见，稳定；重命名视为破坏性变更 |
| `registration_id` | `reg-<n>` | 宿主分配，单调递增；插件不得自拟 |
| `generation` | `<runtime>-<n>` | Sidecar 每次启动递增，如 `node-0007` |
| 错误码 | `E_<SCREAMING>` | 见各规范的错误表 |
| 测试 ID | `<AREA>-<CATEGORY>-<NNN>` | 如 `RPC-CANCEL-003`、`ADC-SLOT-011` |

## 4. 通用约定

- **时间**：毫秒整数；跨进程一律 UTC RFC 3339 字符串（`2026-11-08T10:00:00.000Z`）。
- **大小**：字节整数；文本长度按 UTF-8 字节计。
- **JSON**：禁止 `NaN`/`Infinity`；数字必须是有限值；对象键排序不参与语义（签名除外）。
- **版本**：SemVer；`api` 字段只写 major（`cordis-portable/v1`、`adc/v1`）。
- **单位**：token 数按 provider 计量口径记录，不跨 provider 换算。

## 5. 规范之间的关系

```text
cpp-v1.md  ──┐
             ├─► adc-v1.md  （插件如何向 UI 提交内容）
sidecar-rpc.md ─┘

session-log.md  ◄── sidecar-rpc.md（工具事件写入同一条日志）
session-log.md  ◄── adc-v1.md（回放需要展示元数据）
```

四条规范共享同一组身份：`plugin_id`、`generation`、`registration_id`、`session_id`。任何一条链路缺其中任一项，都视为规范违规。

## 6. 变更流程

1. 提 PR 只改 `docs/spec/`，说明动机、兼容性影响、受影响的 conformance ID。
2. 状态为 Frozen 时，破坏性变更必须先写迁移说明与弃用期。
3. 实现与规范同步落地；**不允许"先实现再补规范"**，除非同时标注为实验能力且不出现在公开文档里。

## 7. 待建规范（不在 M0 范围）

| 计划文件 | 内容 | 时点 |
|---|---|---|
| `context-engineering.md` | 压缩/裁剪/外部化的算法与阈值 | M2 |
| `approval-policy.md` | 审批策略、answerer 链、审计字段 | M2 |
| `acp-bridge.md` | 外部 Agent 接入与状态同步 | M3 |
| `mcp-tools.md` | MCP 工具发现与权限映射 | M2 |
| `plugin-trust.md` | 签名、来源校验、安装风险提示 | M3 |
