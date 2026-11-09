# 契约 Schema

这里放 [`../docs/spec/`](../docs/spec/README.md) 的可执行形态：JSON Schema（draft 2020-12）与校验用例。

| 文件 | 对应规范 | 用途 |
|---|---|---|
| `adc-v1/element.schema.json` | [`adc-v1.md`](../docs/spec/adc-v1.md) §6 | 元素树（递归节点、类型枚举、上限） |
| `adc-v1/slot-registration.schema.json` | [`adc-v1.md`](../docs/spec/adc-v1.md) §4.4 | slot 注册条目 |
| `cpp-v1/manifest.schema.json` | [`cpp-v1.md`](../docs/spec/cpp-v1.md) §3 | `plugin.toml` 的 JSON 投影 |
| `sidecar-rpc/envelope.schema.json` | [`sidecar-rpc.md`](../docs/spec/sidecar-rpc.md) §4 | 控制消息信封 |

## 校验

```powershell
node schema/check.mjs
```

`check.mjs` 内置一个够用即可的 JSON Schema 子集校验器（`$ref`/`type`/`enum`/`const`/`required`/`properties`/`additionalProperties`/`items`/长度/正则/数值范围/`oneOf`·`allOf`·`anyOf`），并对每个 schema 跑正反用例。

**这是临时实现**：JS 工具链 pin 之后换成 `ajv` + `ajv-cli`，用例列表保持不动即可平滑迁移。

## 约定

- Schema 的 `$id` 使用 `https://dshr.local/schema/...`，与线上无关，仅用于引用。
- Schema 与规范冲突时以规范为准，并按 `docs/spec/README.md` §6 的流程改。
- 新增 schema 必须同时新增至少一个"该拒绝的输入"用例——只测通过路径的 schema 没有意义。
