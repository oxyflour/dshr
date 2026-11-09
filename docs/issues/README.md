# 已知问题（docs/issues）

记录**已经确认、暂时不修**的缺陷与限制。每条问题都必须包含：可复现步骤、观察到的现象、已排除/已尝试的方案、下一步修复路径。修好后把状态改为 `Fixed` 并保留条目。

分类：`Platform`（GPUI/OS 行为）、`Contract`（规范/协议缺陷）、`Environment`（本机构建/运行环境）、`Product`（体验问题）。

## 索引

| 文件 | 标题 | 类别 | 状态 | 首次记录 |
|---|---|---|---|---|
| [0001](0001-ime-candidate-first-composition.md) | 中文 IME 首次组合输入时候选窗落在屏幕右下角 | Platform | **Open**（已缓解，未根治） | 2026-11-09 |

## 模板

```markdown
# ISSUE-000N — 标题

- 类别：Platform | Contract | Environment | Product
- 状态：Open | Mitigated | Fixed | Won't fix
- 严重度：阻塞 / 高 / 中 / 低
- 首次记录：YYYY-MM-DD
- 关联：spec/xxx.md、ADR-xxx、commit

## 现象
## 复现
## 根因
## 已尝试
## 下一步
## 影响
```
