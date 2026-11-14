# 决策记录（ADR）

每个文件记录一个**已经做出或正在评估**的决定：背景、选项、结论、后果、验证方式。规范（`../spec/`）说"做什么"，ADR 说"为什么这样选、什么时候能反悔"。

## 规则

1. 一个 ADR 只回答一个决定。改主意时**不删旧文**，新开一篇并把旧的标为 `Superseded by`。
2. 状态只能取：`Proposed`（待验证） · `Accepted`（已采纳） · `Superseded` · `Rejected`。
3. `Accepted` 必须写明**反悔条件**（什么情况下这个决定不再成立）。
4. 与 `docs/PLAN.md` 的 ADR-001…011 是同一套决策的不同粒度：PLAN 里是一句话原则，这里是可以检查的落地细节。

## 模板

```markdown
# ADR-000N — 标题

- 状态：Proposed | Accepted
- 日期：YYYY-MM-DD
- 关联：PLAN §x / spec/xxx.md

## 背景
## 选项
## 决定
## 后果（好消息 / 坏消息）
## 反悔条件
## 验证
```

## 索引

| 文件 | 决定 | 状态 | 时点 |
|---|---|---|---|
| [0001-gpui-pin.md](0001-gpui-pin.md) | 固定 GPUI 与 GPUI Kit 的精确版本 | Accepted | M0 |
| [0002-cordis-rs-selection.md](0002-cordis-rs-selection.md) | Rust Cordis 实现选型 | Accepted | M0 |
| [0003-dsh-pin.md](0003-dsh-pin.md) | 锁定 DSH 版本与 vendored Cordis | Proposed | M0 |
| [0004-adc-slot-contract.md](0004-adc-slot-contract.md) | ADC-v1 采用 slot 注册表而非组件清单 | Accepted | M0 |
| [0005-no-web-shell.md](0005-no-web-shell.md) | 不做 Web 壳捷径 | Accepted | M0 |
| [0006-next-step-native-plugin-registry.md](0006-next-step-native-plugin-registry.md) | M1 收尾顺序：先 Native Plugin + Tool Registry，LLM 往后 | Accepted | M1 |
| [0007-m3-scope.md](0007-m3-scope.md) | M3 范围锁定、M2 后移债务处置、3 条 gate 与砍单顺序 | Accepted | M3 启动 |
| [0008-script-ui-spike.md](0008-script-ui-spike.md) | Script UI 5 日 spike 的时间盒、判定标准与退路 | Accepted | M3 W1 |
| [0009-plugin-trust.md](0009-plugin-trust.md) | 最小信任链：包级 `dshr-trust.json` + 摘要强制 + 可选 ed25519 + 首装确认/审计/降级拒绝 | Accepted | M3 |
| [0010-isolation-runtime-quota.md](0010-isolation-runtime-quota.md) | 隔离运行时配额五维；WASM 主方案因本机离线暂由 `external-process`（Job Object）退路承担 G3 | Accepted（退路启用） | M3 |
| [0011-adc-v1-freeze.md](0011-adc-v1-freeze.md) | ADC-v1 冻结范围、变更政策与已知渲染降级 | Accepted | M3（G1） |
