# M3 Loader 完整语义（include/group）与 DSH 差异

> 更新：2026-11-14 · 实现：[`crates/profile/src/loader.rs`](../crates/profile/src/loader.rs) ·
> 计划依据：[ADR-0007](decisions/0007-m3-scope.md) §3（include/group 进 M3，`!!js` 留 Node）·
> 旧文档：[`m2-loader.md`](m2-loader.md)

## 1. M3 新增能力

| 能力 | 语义 | 入口 |
|---|---|---|
| `include` 递归展开 | 行级 `include: <path>`（字符串/数组）或 `- name: cordis:include` + `config.path(s)`；相对路径基准是**声明文件所在目录**；活动栈防环，循环/缺失/非法路径 → 警告并跳过 | `entries_from_loader_with_base` / `entries_from_loader_files` |
| `group` 展开 | `- group: true` + `config: [rows]`（真实 DSH 形状）或 `group: [rows]`；深度优先展平，嵌套上限 `MAX_GROUP_DEPTH = 16`（超出跳过并告警） | 同上 |
| patch 追加进 group | `insert: [rows]`、`id + insert`（追加进同名 group）、`disabled/config` 覆盖；`PatchOp::InsertInto` | 同上 |
| 诊断 | 统一 `Vec<String>` warnings（include 循环/缺失/逃逸、超深 group、无基准文本入口等） | 两个新入口 |
| 兼容 | `entries_from_loader(text, patches)` 签名不变（文本入口的 include 逐条告警） | 旧入口 |

测试：`M3-LOADER-001..015` + 1 个 lib 集成（`cargo test -p dshr-profile` → 58 passed，原 42）。

## 2. 与真实 DSH Loader 的差异（必须明示）

1. **`!!js` 不求值**：原文透传；`disabled: !!js` 按"真值"处理。**风险**：真实 `dsh-base` patch
   里有多处 `!!js` 条件禁用，会被全部判为 disabled；M3 建议只用于**自有 bundle**，DSH 官方
   bundle 的 `!!js` 仍交给 Node sidecar 处理（ADR-0007 的边界）。
2. **patch 内 `include` 无文件基准**：patch 里的相对路径不锚定 patch 文件所在目录（真实 loader 有）；
   `name` 相对路径也不锚定。→ 跨文件 patch 需写绝对/仓库相对路径，文档标注。
3. **group 的 `config` 与 `children` 双存**：patch 覆盖 `group.config` 不会重建 children；
   对真实 DSH 的"覆盖后即时生效"语义是超集/差异，文档标注。
4. **`isolate` / `intercept` 不生效**：真实 Loader 的隔离/拦截语义未实现（M4+）。
5. **深度上限 16**：为防滥用新增（告警非报错），真实 loader 无此限制。
6. **超集**：支持 JSON include 与 bundle 顶层 `include` 映射（真实方言之外）。
7. **patch 可按 id 覆盖 group 内条目**：真实 DSH 未承诺；作为我们的超集记录。

## 3. 使用建议

- 自有 bundle（项目/团队维护）：可用 include/group/patch，注意 §2 第 1/2 条。
- 官方 DSH bundle：`!!js`、嵌套 patch、isolate 语义仍由 Node sidecar 原生 Loader 处理；
  Rust 侧只做"派生只读视图"（M2 行为）时不要开启完整展开，或只读取 `plugins.json` 派生结果。
- 诊断：`entries_from_loader_with_base` 返回的 warnings 必须展示给用户（CLI/GUI），
  尤其是 include 缺失与 `!!js` 跳过。

## 4. 后续（M4+）

- 评估 `!!js` 的安全求值（受限 JS 引擎）或永久留给 Node；
- patch 文件基准路径与 name 锚定；
- `isolate`/`intercept` 与真实 DSH 语义对齐。
