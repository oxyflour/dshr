# Loader YAML 支持范围（include、group、patch）

桌面端设置 `DSHR_LOADER_BUNDLE` 后，由 Rust 读取 bundle 和可选的 `DSHR_LOADER_PATCH`，派生只读
profile。桌面默认使用的 DSH sidecar 接收启用条目的 `name`、`config`、`inject`，
sidecar 按条目导入模块并调用 `ctx.plugin`。调整全局条目后重启生效。默认的文件模式使用 `plugins.json`。
[Profile 实现](../crates/profile/src/lib.rs) · [Sidecar 实现](../sidecar/dsh-official/sidecar-dsh.mjs)

基础发行包内置 Node 和默认官方 Sidecar 依赖。Windows x64 的默认工作区依赖工具需要可用的 Python 载荷。可通过「设置 → 运行组件」安装；在 Loader 或 `plugins.json` 显式指定 `config.source` 时使用指定的 DSH `primary-runtime` 路径。[运行组件规范](spec/runtime-components.md)

## 支持的配置

| 配置 | dshr 行为 |
|---|---|
| bundle 内的 `include` | 支持 `include: <path>`、路径数组，以及 `name: cordis:include` 配合 `config.path` 或 `config.paths`。相对路径以声明该 include 的文件所在目录为基准。也支持顶层 `include` 映射和 JSON include |
| `group` | 支持 `group: true` 配合 `config: [rows]`，也支持 `group: [rows]`。将子条目按深度优先顺序展开 |
| patch 插入 | `insert: [rows]` 追加顶层条目。`id` 和 `insert` 同时指定时，追加到对应 group |
| patch 覆盖 | 按 `id` 覆盖普通条目或 group 内的子条目。`config` 整体替换，`disabled` 和 `inject` 可覆盖。`name` 用于断言原名称 |

`include` 和 `group` 的深度上限均为 16。缺失文件、循环引用、非法 include 路径和超深分支会告警并跳过。
文本入口 `entries_from_loader` 缺少 include 的文件基准，带文件路径的桌面入口使用 `entries_from_loader_files`。
预设后台重载遇到缺失或非法的 include 会保留上一次已发布的目录。空 patch 可以写作 `[]`。
[Loader 实现及 `M3-LOADER-001..015` 测试](../crates/profile/src/loader.rs)

## 迁移 DSH 配置时的限制

具名 headless 使用静态声明入口，覆盖优先级和支持字段见 [Headless CLI 规范](spec/headless-profile.md#7-dsh-profile-声明兼容)。

- **`!!js` 不求值。** 表达式按字符串保留，`disabled: !!js` 的非空表达式会按真值禁用条目。把条件替换成已经求值的布尔值或配置字面量。
- **`isolate` 和 `intercept` 尚未实现。** bundle 条目可以包含这些字段，派生的 profile 不携带对应语义。依赖服务隔离或拦截的插件需要适配。
- **patch 插入的 include 会告警并跳过。** 把 include 放在 bundle 里。插件 `name` 的相对路径也不以 YAML 文件为基准，可使用 sidecar 可解析的包名或绝对 `file:` URL。
- **覆盖 `group.config` 不会重建子条目。** 用 `id` 覆盖具体子条目，或通过 `id` 和 `insert` 追加子条目。
- **预设与全局条目分别装载。** 声明 `@deepseek-ai/dsh-agent-preset`，用 `config.id` 标识预设、`config.plugins` 列出子插件。用 Loader 行 ID 覆盖整份 `config`。桌面后台检查 bundle、patch 和 include，空闲时自动发布稳定变更。已绑定会话保留原修订。全局条目仍由 profile 启动配置装载。
  [Agent 预设契约](spec/agent-presets.md)

官方 DSH bundle 可以作为选择插件条目和配置的参考。迁移时把所需条目写入 dshr 自有 bundle 或 `plugins.json`，
再按包版本核对实际可执行的工具和所需宿主服务。[DSH 插件兼容矩阵](spec/dsh-compat-mapping.md#兼容矩阵)

## 声明创造预设

将下面的行加入 `DSHR_LOADER_BUNDLE` 指向的 YAML。示例需要先准备兼容矩阵列出的 `dsh-tool-working-directory` 包，并启用 Sidecar 服务代理。在新会话的模式菜单中选择「创造模式」即可使用 `cordis`。需要覆盖组合时，在 patch 中按 `preset-cordis` 替换整份 `config`，包括 `id` 和完整 `plugins`。

```yaml
- id: preset-cordis
  name: '@deepseek-ai/dsh-agent-preset'
  config:
    id: cordis
    plugins:
      - id: working-directory
        name: '@deepseek-ai/dsh-tool-working-directory'
```

## 验证

`dshr --selftest-loader` 覆盖 bundle 与 patch 的派生、禁用条目过滤和只读 profile。
Loader 解析失败时，桌面端记录错误并回退到默认 profile。遇到插件未注册时，先检查 Loader 诊断和实际启用的条目。
[桌面自检与启动入口](../apps/desktop-gpui/src/bin/chat.rs) · [M2 历史记录](m2-loader.md)
