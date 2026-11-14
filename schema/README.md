# 契约 Schema

这里放 [`../docs/spec/`](../docs/spec/README.md) 的可执行形态：JSON Schema（draft 2020-12）与校验用例。

| 文件 | 对应规范 | 用途 |
|---|---|---|
| `adc-v1/slots.v1.json` | [`adc-v1.md`](../docs/spec/adc-v1.md) §5 | slot 目录 v1（12 slot、kind/scope/注册契约/key 域），**唯一事实源**；数据文件，不是 JSON Schema |
| `adc-v1/element.schema.json` | [`adc-v1.md`](../docs/spec/adc-v1.md) §6 | 元素树（递归节点、26 元素字段级 props、类型枚举、上限） |
| `adc-v1/slot-registration.schema.json` | [`adc-v1.md`](../docs/spec/adc-v1.md) §4.4 | slot 注册条目（view/run × unkeyed/keyed 四种载荷形状） |
| `cpp-v1/manifest.schema.json` | [`cpp-v1.md`](../docs/spec/cpp-v1.md) §3 | `plugin.toml` 的 JSON 投影（含 fuel 上限、`min_host_version`、`ui[].location`） |
| `cpp-v1/trust.schema.json` | [`plugin-trust.md`](../docs/spec/plugin-trust.md) §2 | 包级信任文件 `dshr-trust.json`（摘要 + 可选 ed25519 签名） |
| `sidecar-rpc/envelope.schema.json` | [`sidecar-rpc.md`](../docs/spec/sidecar-rpc.md) §4 | 控制消息信封（L2 host-service 代理 kind 与 `service_key`） |

> `slots.v1.json` 的字段级必填（如 `workspace.panel` 的 `label`/`location`）只存在于**目录数据**里，
> 通用 JSON Schema 不知道 `slot_id`；`check.mjs` 用目录做第二层契约校验。作者期 lint 与运行时
> 降级是两件事：schema 严格、运行时空（见 `adc-v1.md` §6.1 与 §11）。

## 校验

```powershell
node schema/check.mjs
```

`check.mjs` 内置一个够用即可的 JSON Schema 子集校验器（`$ref`/`type`/`enum`/`const`/`required`/`properties`/`additionalProperties`/`items`/长度/正则/数值范围/`oneOf`·`allOf`·`anyOf`），并对每个 schema 跑正反用例；另对 `slots.v1.json` 做目录自检与 kind 级注册契约校验（当前 57 条用例）。

**这是临时实现**：JS 工具链 pin 之后换成 `ajv` + `ajv-cli`，用例列表保持不动即可平滑迁移。

## 约定

- Schema 的 `$id` 使用 `https://dshr.local/schema/...`，与线上无关，仅用于引用。
- Schema 与规范冲突时以规范为准，并按 `docs/spec/README.md` §6 的流程改。
- 新增 schema 必须同时新增至少一个"该拒绝的输入"用例——只测通过路径的 schema 没有意义。
