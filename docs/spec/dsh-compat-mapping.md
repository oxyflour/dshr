# DSH 插件兼容映射

dshr 通过 Node sidecar 复用部分 DSH 工具包。兼容性取决于包版本、配置、所需服务和实际调用路径。
dshr 自有插件使用 [CPP-v1](cpp-v1.md)，界面扩展使用 [ADC-v1](adc-v1.md)。

新会话工作区旁的「创造模式」选择可覆盖的 `cordis` 预设，支持静态子插件组合、修订和后台配置热重载。dshr 提供 ADC-v1 预览与 CPP-v1 试用工具。
`creator_inspect` 提供 dshr 的 CPP-v1 清单和可构建的交互面板示例。`creator_build_plugin` 通过 Cargo 构建 Rust 外部进程包，并为构建提供同版本 SDK。`creator_try_plugin` 校验声明的工作区面板并支持在会话内重新试用同一包。试用面板在创造会话的消息列表上方显示。Windows x64 可按需安装随发行配套的 Cargo、链接器和离线 SDK 依赖。原版完整的创造工具组合、`cordisInspect` 和插件管理仍需适配。[Agent 预设契约](agent-presets.md) · [dshr 创造模式契约](creator-mode.md) · [交互面板验收](../plan/creator-mode.md#交互面板故障复盘)
具名 headless 的 DSH 清单和补丁规则见 [Headless CLI 规范](headless-profile.md#7-dsh-profile-声明兼容)。

## 兼容矩阵

下表记录实测的包版本和操作。**已验证执行**表示所列操作返回了预期结果或完成了相应的文件、会话或存储变更。
**仅验证注册**表示观察到了对应的 `tool.register`。**部分可用**表示同一包中有已验证操作，也有明确的能力缺口。
**静态声明兼容**表示 dshr 识别相应声明并执行表中列出的行为。**需要适配**表示现有加载入口或服务契约无法满足所列用法。

所有行均使用 `@deepseek-ai/` 包名前缀。桌面默认启动官方 DSH sidecar，并按已启用的 profile 条目注册工具。
默认 profile 包含 `dsh-tool-workspace-dependencies` 和 `dsh-tool-fs`。
其他包需先准备 Sidecar 可解析的包及依赖，再在 `plugins.json` 或 Loader 派生的 profile 中显式配置。
[默认插件清单](../../crates/profile/src/lib.rs#L215) · [Loader 支持范围](../m3-loader.md)

| 包 | 版本 | 状态 | 已验证的操作与边界 | 启用条件 | 测试证据 |
|---|---|---|---|---|---|
| `dsh-tool-workspace-dependencies` | `0.2.2-rc.1` | 已验证执行 | `load_workspace_dependencies` 返回 DSH 运行时路径。干净 Windows 来宾安装组件后，从工具返回的路径运行了 CPython `3.13.12` 并导入标准库 | 默认条目。需要可用的 DSH `primary-runtime`，来源为外部 DSH 运行时或应用安装的 Python 组件。组件仅含解释器和标准库 | [只读自检记录](../m2-dsh-official.md#证据) · [干净来宾验收](../plan/runtime-components.md#干净来宾验收结果) |
| `dsh-tool-fs` | `0.2.2-rc.1` | 部分可用 | `read`、`write`、`edit`、`apply_patch` 已验证。干净 Windows 来宾在仅有基础包时复验了 `read`。写入目标的父目录必须已存在。`read_image` 依赖尚未接入的附件服务 | 默认条目补充注入 `tools`、`fs`、`systemPrompt`。写操作需启用对应开关、协商 `fs.write` 并通过宿主审批 | [`RPC-L2-506/507`，读取和写入](../../crates/compat-dsh-host/tests/official_packages.rs) · [编辑、补丁、审批拒绝与缺失父目录](../../crates/compat-dsh-host/tests/fs_write_e2e.rs) · [干净来宾验收](../plan/runtime-components.md#干净来宾验收结果) |
| `dsh-tool-todo` | `0.2.2-rc.1` | 已验证执行 | `todo_write` 写入 `todo/write` 会话事件 | 显式配置 `allowParallelInProgress` 布尔值，协商 v2 `service.proxy`，使用会话日志与投影服务 | [`RPC-L2-201`](../../crates/compat-dsh-host/tests/official_packages.rs) |
| `dsh-tool-working-directory` | `0.2.2-rc.1` | 已验证执行 | 全局和预设内的 `working_directory` 均可切换会话目录。工作目录服务的 `get`、`ensure`、`set` 已验证往返。官方 `fs` 工具仍以 dshr 工作区根解析相对路径 | 显式加入全局 profile 或预设的 `config.plugins`，协商 v2 `service.proxy` | [`RPC-L2-501/502/503`](../../crates/compat-dsh-host/tests/official_packages.rs) · [启用示例](../m3-loader.md#声明创造预设) |
| `dsh-tool-pwsh` | `0.2.2-rc.1` | 需要适配 | 源码声明注入 `shell`、`shellEnv` 等服务。工具运行尚未验证 | 需要接入官方 Shell、环境、审批和隔离服务 | [依赖调查](../m3-l2-services.md) |
| `dsh-agent-preset`、`dsh-agent-preset-registry` | `0.2.2-rc.1` | 静态声明兼容 | dshr 解析预设 ID、默认值和子插件列表。后台发布 bundle、patch 与 include 的变更，保留会话和子会话已绑定的旧修订 | 官方 Sidecar 与 `DSHR_LOADER_BUNDLE`。由 dshr 实现注册表 | [`M3-PRESET-001..005`](agent-presets.md#验收) |
| `dsh-tool-cordis` | `0.2.2-rc.1` | 需要适配 | 官方只读检查工具依赖 `cordisInspect` Host provider 与 Agent 执行上下文 | 需要接入官方检查服务 | [依赖调查](../m3-l2-services.md) |
| `dsh-storage-domain` | `0.2.2-rc.1` | 已验证执行 | 域经宿主后端完成打开、KV 读写、删除和关闭，重复打开返回 `already-open` | 显式加入插件，令 `config.backend` 为 `storage-kv` 并协商 v2 `service.proxy` | [`RPC-L2-504`](../../crates/compat-dsh-host/tests/official_packages.rs) |
| `dsh-storage` | `0.2.2-rc.1` | 需要适配 | 包导出 Cordis Service class，现有加载入口接收 `apply` 插件。当前的 `ctx.storage` 由 sidecar 代理提供 | 直接加载该 Service class 需要扩展宿主启动逻辑 | [`RPC-L2-505`](../../crates/compat-dsh-host/tests/official_packages.rs) |
| `dsh-tool-workspace-dependencies` | `0.2.2-rc.5` | 已验证执行 | `load_workspace_dependencies` 返回 Python 路径 | 使用来自本机 DSH 的 `primary-runtime` | [本地冒烟记录](#rc5-本地冒烟) |
| `dsh-tool-fs` | `0.2.2-rc.5` | 已验证执行 | `read` 返回测试文件内容。`write`、`edit`、`apply_patch` 尚未在此版本复验 | 补充注入 `tools`、`fs`、`systemPrompt`，探针启用 `read` | [本地冒烟记录](#rc5-本地冒烟) |
| `dsh-tool-todo` | `0.2.2-rc.5` | 仅验证注册 | 注册 `todo_write`。尚未复验写入会话事件 | 显式配置 `allowParallelInProgress` 布尔值并协商 v2 `service.proxy` | [本地冒烟记录](#rc5-本地冒烟) |
| `dsh-tool-working-directory` | `0.2.2-rc.5` | 仅验证注册 | 注册 `working_directory`。尚未复验目录切换 | 显式加入插件并协商 v2 `service.proxy` | [本地冒烟记录](#rc5-本地冒烟) |
| `dsh-tool-str-replace-editor` | `0.2.2-rc.5` | 部分可用 | `str_replace_editor` 的 `view` 可查看普通文件。查看目录因缺少 `ctx.fs.listDir` 失败，写入子命令尚未验证 | 显式加入插件，使用 sidecar 的 `tools` 和 `fs` | [本地冒烟记录](#rc5-本地冒烟) |

`0.2.2-rc.1` 的依赖调查显示，官方 `dsh-tool-bash` 和 `dsh-tool-pwsh` 需要 `shell` / `shellEnv`，`dsh-tool-fs-search` 的
`glob` / `grep` 需要 `subprocess`，`dsh-tool-web` 和 `dsh-tool-subagent` 分别需要 `web` 和
`subagents`。这些服务尚未接入当前 sidecar。第三方包应同时检查 `inject`、`apply` 和 `execute`
访问的服务方法与执行上下文。[官方工具依赖调查](../m3-l2-services.md)

## 启用条件与适配边界

基础包带 Node 与 Sidecar。Python 与创造模式构建能力分别由可选组件提供。[运行组件契约](runtime-components.md)

- **桌面启动。** `DSHR_SIDECAR` 未设置或为空时，桌面使用随应用提供的官方 DSH sidecar。`probe` 与 `cordis` Sidecar 仅供显式诊断命令使用。产品运行时保留 `probe_*` 前缀给诊断工具，
  模型工具清单和派发均排除这些名称。桌面产品归档只随附官方 DSH sidecar。官方 sidecar 缺少依赖或启动失败时，桌面显示加载错误并继续提供已加载的原生工具。
  headless 的启动失败条件见[运行配置与工具](headless-profile.md#3-运行配置与工具)。桌面和 CLI 的工具隔离有 [`CPP-CREATOR-009`](creator-mode.md#兼容性与验收) 回归覆盖。
- **配置与装载。** sidecar 导入条目中的 `name`，解析插件的 `Config`，将 `apply` 交给 Cordis。
  在 `plugins.json` 的 `plugins` 数组中按 `name`、`enabled`、`config` 和可选的 `inject` 添加条目。
  桌面从 profile 生成 `DSHR_DSH_PLUGINS`，直接运行 sidecar 时可通过该环境变量传入已启用的条目数组。
  `tools/dsh-vendor.mjs` 默认抽取本机安装版中的工作区依赖工具、文件工具及其依赖。可以用 `DSHR_ASAR`
  选择其他安装包，用脚本参数指定抽取目录和所需工具包。
  [Sidecar 装载代码](../../sidecar/dsh-official/sidecar-dsh.mjs#L724) · [抽取脚本](../../tools/dsh-vendor.mjs)
- **文件与会话服务。** `fs.write` 协商成功后，写请求回到 Rust 做工作区检查、审批、写前快照和写入。
  v2 `service.proxy` 另行协商，提供工作目录、会话日志、会话投影和 KV 存储的适配方法。
  桌面端可用 `DSHR_FS_WRITE=0` 关闭写能力，用 `DSHR_HOST_SERVICES=0` 关闭服务代理。
  `exec.agent` 的最小会话接口只向 `working_directory` 和 `todo_write` 提供。`systemPrompt` 适配器收集段落，
  尚未接入 Rust 的提示词组装。[宿主能力开关](../../apps/desktop-gpui/src/bin/chat.rs#L1401) · [v2 服务方法表](sidecar-rpc.md#132-service_key-与方法表)
- **工具作用域和身份。** 已验证的官方包按工作区登记全局工具。桌面也支持 Sidecar 以显式 `session_id` 登记工具，以及
  创造会话中的原生试用工具。会话定义优先于同名全局定义。切换会话保留试用实例，返回工作区时复用仍在运行的
  Sidecar。每次模型请求固定可用的注册，撤销后旧身份无法调用同名替代工具。
  [Sidecar 契约](sidecar-rpc.md#注册作用域和调用身份) · [作用域与撤销测试](../../crates/compat-dsh-host/tests/official_packages.rs)
- **执行和权限。** Sidecar 调用 `definition.execute` 并传入可取消的 `signal`。DSH 执行钩子与
  `isConcurrencySafe` 调度语义尚未接入。
  `restrict`、`guard`、`presentAs` 是空实现。依赖这些能力实施访问控制的插件需要适配宿主策略。
  [工具注册与调用代码](../../sidecar/dsh-official/sidecar-dsh.mjs#L157)
- **界面。** DSH 客户端挂载点和自定义 UI 需用 ADC-v1 重建。[ADC-v1 规范](adc-v1.md)

### 工具字段映射

| DSH 字段或回调 | 当前 sidecar 行为 |
|---|---|
| `name`、`description`、`parameters`、`output.schema`、`timeoutMs` | 映射到 `tool.register` 的 `name`、`description`、`input_schema`、`output_schema`、`timeout_ms` |
| `execute`、`output.render`、`output.presentationMeta` | 结果值进入 `value`，输出回调的结果分别进入 `content` 与 `meta`。没有可用的内容数组时回退到 JSON 内容块 |
| `presentCall`、`presentResult` | 调用和结果视图回调尚未桥接。官方路径以 `generic` 注册工具 |
| `deferLoading`、`isConcurrencySafe` | 上游工具的懒加载和并发判定尚未接入 |
| `projectContent`、`finalizeContent`、`additionalContexts`、`concludesTurn` | 内容投影与收尾回调、向对话追加上下文和结束轮次尚未桥接 |

[注册字段转换](../../sidecar/dsh-official/sidecar-dsh.mjs#L157) · [调用与结果转换](../../sidecar/dsh-official/sidecar-dsh.mjs#L791)

DSH 自身的 Host/Client Remote RPC 和会话格式各有独立契约。dshr 的 Rust 与 Node 之间使用 stdio 上的
NDJSON，Rust 保存自己的会话事件，通过 v2 服务向插件公开所需的会话操作。
[Sidecar RPC](sidecar-rpc.md) · [dshr 会话日志](session-log.md)

## 验证与复现

### rc.1 集成测试

用测试环境中实际加载的 `package.json` 核对包版本。仓库的来源锁记录 todo、工作目录和 storage-domain
三个官方包的版本及摘要。当前 `runtime.ready.dsh_version` 是固定值，核对抽取版本时以包清单为准。
[官方包来源锁](../../sidecar/dsh-official/official-packages.lock.json) · [握手实现](../../sidecar/dsh-official/sidecar-dsh.mjs#L976)

准备 `0.2.2-rc.1` 的抽取物后，可在 Windows 运行下列命令。准备待办和工作目录测试包的方法见 [官方包测试准备](../m3-l2-services.md#9-复现命令)。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host --test fs_write_e2e -- --nocapture
```

官方包 gate 覆盖 `RPC-L2-201`、`RPC-L2-501..507`、`RPC-REG-001` 和 `M3-PRESET-002/003`。普通 `cargo test` 中这些项目标为 `ignored` 时，
表示本次未执行。文件写入测试还需确认实际输出含注册结果且无 `SKIP`。`dshr --selftest-dsh` 验证默认桌面
运行时注册工作区依赖工具和 `read`，并验证读取测试文件和会话回放。打包用的 `dshr --selftest-official` 使用空插件列表验证
sidecar 与 v2 握手。[Conformance 的人工验收口径](../conformance.md#7-与-m3-exit-gate-的对应) · [打包自检实现](../../apps/desktop-gpui/src/bin/chat.rs#L12480)

### rc.5 本地冒烟

2026-11-21 在 Windows 从本机 DSH `0.2.2-rc.5` 安装包抽取上表五个工具包到隔离目录，按各包的
`package.json` 确认版本。首轮使用 dshr `b9ccba4` 的 Sidecar，随后用启用作用域和注册身份的代码复验。
两次都协商 v2 `service.proxy`，未开启 `fs.write`。复验按宿主确认的 `registration_id` 发起调用。

| 检查 | 观察结果 |
|---|---|
| `tool.register` | `load_workspace_dependencies`、`read`、`todo_write`、`working_directory`、`str_replace_editor` |
| 调用 `load_workspace_dependencies` | 返回 Python 路径 |
| 调用 `read`、`str_replace_editor(view)` | 均返回普通文件的内容 |
| 对目录调用 `str_replace_editor(view)` | `ctx.fs.listDir is not a function` |
| 向 `read` 传入不存在的 `registration_id` | `E_TOOL_UNAVAILABLE` |

复验时使用 `node tools/dsh-vendor.mjs <隔离目录>/node_modules <包名...>` 抽取依赖，把
`sidecar-dsh.mjs` 和 `lib/` 复制到隔离目录。在直接启动的 sidecar 进程中设置 `DSHR_DSH_PLUGINS`
与 `DSHR_WORKSPACE`，按矩阵配置插件。工作区依赖工具的 `config.source` 指向 DSH 的 `primary-runtime` 绝对路径。
发送带 `service.proxy` 的 v2 `runtime.hello`，逐条确认 `tool.register` 并分配 ID。收到启动日志后，
使用对应的 `plugin_id`、`registration_id`、`session_id` 和 generation 发出 `tool.invoke`。
[抽取脚本](../../tools/dsh-vendor.mjs) · [协议消息格式](sidecar-rpc.md#4-envelope)

更新矩阵时记录实际包版本、验证的工具或服务操作、启用条件和测试出处。升级抽取物后保留原版本记录，
按新版本的实测结果补行。[DSH 版本漂移记录](../decisions/0003-dsh-pin.md)
