# Loader YAML 兼容（2026-11-11）

上一轮插件集合来自 `plugins.json`（手写条目）。这一轮补 Loader 方言：
从 `cordis.yml`（bundle）+ `cordis.patch.yml`（patch 层）派生条目。

## 支持的方言

参考 `cordis-composition-reference`，实现的是**够用子集**：

- bundle：顶层条目列表，行字段 `id` / `name` / `config` / `inject` / `disabled` / `group`；
- patch：每项是 `insert: [rows]` 或带 `id` 的覆盖映射；
  - 覆盖时 `config` **整体替换**（不深合并），`disabled` / `inject` 可改；
  - 覆盖 `name` 视为"断言名字"：与现有条目不一致会告警并忽略；
  - 目标 `id` 不存在 → 告警跳过；
- 标量：引号字符串、布尔、整数、`null`、`[a, b]` 数组、`!!js`（按字符串保留，不求值）；
- 注释、空行、`---` 分隔符。

尚未支持（会告警/跳过，不静默丢）：

- `group: true` 的嵌套条目与"向 group 插入"（当前只保留顶层条目）；
- `cordis:include`（引用外部 YAML/JSON 列表）；
- `isolate` / `intercept` 的服务实例语义；
- `!!js` 求值。

## 与 profile 的关系

- `ProfileSource::File`：`plugins.json`，可编辑（上一轮的 UI 启停）；
- `ProfileSource::Loader`：由 `DSHR_LOADER_BUNDLE`（bundle 路径）与可选
  `DSHR_LOADER_PATCH`（patch 路径）派生，**只读**：
  - 不写 `plugins.json`（避免把派生结果固化成第二真相）；
  - 插件面板的按钮变为"只读"，侧栏显示"Loader 只读"；
  - 改 bundle/patch 后重启生效。

启动顺序：加载 profile（File 或 Loader）→ 设置 `DSHR_DSH_PLUGINS` → 启动 sidecar。

## 自检

- `profile` 9 个测试：原有 4 个 + loader 5 个（bundle 解析、patch 覆盖/插入、
  group 跳过、缺失目标告警、标量/注释/引号）。
- `--selftest-loader`：写一份 bundle + patch，断言 fs 被禁用、extra 被插入、
  `sidecar_json()` 不含禁用项、profile 只读且 `save()` 拒绝。
- `--selftest-profile` 继续覆盖 File 模式与 sidecar 端到端。

## 边界

这是"读 Loader 的组合文件"，不是完整 Loader：`include`、group 展开、`!!js`
求值、`isolate` 都需要 DSH 的 Loader 运行时语义，当前不实现。
安装流程（npm/离线 vendor + 签名）仍未做，见 [`m2-plugins.md`](m2-plugins.md)。
