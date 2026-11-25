# DSH 客户端槽位兼容计划

> 原生槽位已接线，发行验收待完成 · 2026-11-25 · 对照 DSH Desktop `0.2.2-rc.5`，Windows x64 优先。

建议先交付 **原生挂载位置兼容**。插件保留可复用的工具和业务逻辑，视图改为 CPP-v1 提供的 ADC 元素树。Windows 试点覆盖 **对话卡内的交互面板、普通工具卡的声明式视图、右栏插件页签**，估算 **18–27 人日**。加入输入区、设置、中央页面和发行验收后，原生首版合计 **32–48 人日**。原版 React 插件的隔离运行属于 L4，另设验证阶段。 [项目路线图](../PLAN.md#54-dsh-client-plugin-的处理)

## 基线与取舍

- 实施前，ADC 的 12 个槽位均有目录和注册契约，通用注册表已实现字段校验、解析和撤销。桌面通过独立的 CPP-v1 面板路径接通 `workspace.panel`，在右侧栏「插件视图」、插件页「插件面板」和试用结果卡显示贡献。注册时未保留声明中的 `location`。 [ADC 注册表](../../crates/ui-contributions/src/registry.rs) · [桌面面板路由](../../apps/desktop-gpui/src/bin/chat/runtime/state.rs) · [右侧栏实现](../../apps/desktop-gpui/src/bin/chat/session_header.rs) · [试用规则](../spec/creator-mode.md#试用与失败条件)
- 实施前，工具结果只通过 `meta.ui` 提供只读 ADC 附加区。 [工具卡实现](../../apps/desktop-gpui/src/bin/chat/view/transcript.rs) · [槽位注册契约](../spec/adc-v1.md#5-slot-目录-v1)
- DSH 的生成目录包含 116 个客户端槽位，实际可用项取决于当前 Client 挂载的包。每项有各自的作用域、键域、注册参数和替换语义。估算按用户可见的交付范围拆分，复用现有 ADC 槽位时仍需核对契约。 [官方槽位目录](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/extensions/cordis-client-runner/src/client/slot-catalog.ts) · [ADC 槽位目录](../spec/adc-v1.md#5-slot-目录-v1)
- 原版 DSH 的 `Slots` 查询能返回客户端的活动槽位。dshr 的官方检查工具当前只登记 Host 的 `Tool` 和 `HostService` 查询。首版从桌面的实际挂载能力生成 `creator_inspect` 指引。原版 Client 出现后再向 Agent 暴露该 Client 的实时 `Slots` 查询。 [dshr 检查适配](../../sidecar/dsh-official/lib/cordis-inspect.mjs) · [DSH 查询契约](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/docs/tool-catalog.md#deepseek-aidsh-tool-cordis)

## 挂载范围

| 顺序 | DSH 槽位或位置 | dshr 交付方式 |
|---|---|---|
| 试点 | `tool.view.cordis`，运行结果卡 | 复用 `workspace.panel` 和 `creator_try_plugin` 卡片。保留同一插件只在最近一次成功试用卡内挂载活动视图的行为。 |
| 试点 | `tool.call.toolview`，按工具名显示 | 接通同名 ADC 槽位。允许同一插件为自己的工具提供声明式视图，渲染失败时回退原工具卡。试点验证结果展示和安全宿主动作。 |
| 试点 | `sidebar.right.pane.tab`，右栏页签 | `workspace.sidebar.tab` 添加会话级页签，复用现有右侧栏。 |
| 首版扩展 | `conversation.input.left`、`conversation.input.right` | 接通已有 ADC 槽位，使用紧凑控件。输入区上下方和卡内浮层待后续评估。 |
| 首版扩展 | `settings.section`、`settings.general.item` | 接通已有 ADC 槽位。配置读写复用已授权的插件接口。 |
| 首版扩展 | `sidebar.panellist` 与 `main` | `workspace.page` 同时注册左侧入口和中央页面。 |
| 首版扩展 | `sidebar.right.tab.document` | `viewer.tab` 为当前会话的交付文件提供右栏预览。 |
| 首版扩展 | `conversation.chat.node` | `conversation.chat.node` 按插件发布的节点类型在对话流挂载独立视图。以调用序号区分实例，用日志中的文本回放。 |
| 后续按用例评估 | `conversation.view`、`conversation.chat.turnTail`、`plugins.tab`、`shell.bottom`、`shell.overlay` | 各位置按真实客户端包和使用场景估算。 |

老包应继续通过 `workspace.panel` 校验，并按安装或试用方式显示在插件页或试用结果卡。右栏页签和中央页面用明确的槽位声明，`creator_inspect` 说明历史 `location` 值的实际效果。先在 ADC 与 CPP-v1 相关规范里记录包的 opt-in 方式、作用域、冲突、失败条件和回退。同步 Schema、规范索引和契约测试后再接实现。 [ADC 版本规则](../spec/adc-v1.md#9-版本与兼容) · [CPP-v1 面板声明](../spec/cpp-v1.md#123-声明processready)

## 实施顺序与估算

按一名熟悉仓库的工程师估算，包含规范、代码、测试和 Windows 发行验收。试点包括一个跨位置的 CPP-v1 示例，以及一次代表性 DSH 视图的 ADC 移植。范围以一个锁定的 DSH 版本为准。各阶段的上限覆盖注册与桌面作用域联调，阶段结束后用实际耗时重估。

| 阶段 | 人日 | 依赖 | 交付和通过条件 |
|---|---:|---|---|
| 0. 锁定试点 | 3–5 | 可取得 `rc.5` 官方包或源码 | 固定来源、版本和摘要。为三个试点位置记录上游的实际注册契约、所需属性及会话归属。选一个有实际交互的视图，验证其宿主依赖，更新规范和 Schema 草案。 |
| 1. 接通注册与发现 | 7–10 | 阶段 0 | 让 CPP-v1 宿主入口使用 ADC 注册校验和解析规则。沿用已安装与试用实例的生命周期，用作用域和注册身份约束渲染与动作。注册撤销、同名冲突、切换会话、失败重试各有自动断言。由同一能力数据驱动 `creator_inspect`、试用结果和未知槽位诊断。 |
| 2. Windows 试点 | 8–12 | 阶段 1 | 回归现有试用卡并打通自定义工具卡和右栏页签。在发行窗口实测首渲染、点击及键盘、刷新、切换工作区、关闭页签和卸载。复跑一个代表性视图的迁移，分别记录哪些 Host 能力可复用、哪些视图代码需要改写。 |
| 3. 扩展三组挂载 | 10–15 | 阶段 2 | 接通输入区左右、设置页和设置项、左侧导航与中央页面。覆盖窄窗、焦点进出、内建控件共存、错误回退和实例撤销后的页面退出。 |
| 4. 首版发行验收 | 4–6 | 阶段 3 | 运行 Schema、契约、现有官方包 gate、严格 Clippy 和 Windows 发行回归。每项以真实窗口和已验证的包记录结果。用一个真实模型任务确认其能先找到支持的位置，再注册并试用到相同位置。补齐兼容矩阵与作者指南。 |

**阶段 0–2 合计 18–27 人日，阶段 0–4 合计 32–48 人日**，约 6.5–10 个工程师周。Linux 和 macOS 的发行窗口实测另估 **5–9 人日**，取决于打包、输入和焦点差异。现有三平台质量门禁仍随代码变更执行。

## 验收要求

- **Agent 查到的能力与桌面一致。** `creator_inspect` 只把实际接通的槽位列为可用，给出注册参数、作用域和实际位置。试用成功结果按面板报告位置，未知或当前不可用的位置有可操作的诊断。用同一个测试包比较发现结果、注册结果和窗口挂载。
- **动作不能越过注册边界。** 至少用两个会话和两个插件验证同名 view、切换会话、失败后保留旧实例、替换后拒绝过期动作。插件卸载后对应入口消失。历史工具结果还能回放，只有当前活动注册可以接收动作。只读预览沿用原路径。
- **渲染可回退。** 视图请求在运行时执行，GPUI 渲染读取缓存。插件超时或返回非法树时，其他面板和聊天继续工作，工具结果保留原生可读内容。用现有 ADC 配额和构造预算作回归，并在 Windows 窄窗验证滚动、焦点恢复和内建审批交互。
- **按兼容程度登记。** 矩阵逐项列出 DSH 版本、dshr 槽位、运行平台、测试包和行为范围。原生适配记录为视图已移植。官方未改客户端包只有通过实际装载、挂载、交互和卸载后才登记为 L4 已验证。 [现有兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵)

DSH `0.2.2-rc.5` 在 `doc-sync` 检查生成的槽位目录，在 Vitest 验证注册与撤销，在 jsdom 用生产注册表和渲染器挂载特性插件，用 Remote mock 启动整套客户端，再用 Playwright 操作组装后的 Web 客户端。右栏的局部用例覆盖同类型页签实例隔离、收起和重新挂载。浏览器场景断言键盘焦点、分栏、关闭与重开，并比较 ARIA 快照。按同样的测试粒度，dshr 用多槽位 fixture 检查目录、Agent 发现和注册回报，用独立窗口场景覆盖各区域独有的交互。 [上游目录生成说明](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/extensions/cordis-client-runner/src/client/slot-catalog.ts#L1-L15) · [上游注册测试](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/client/ui-renderer/tests/registry.client.spec.ts#L264) · [上游客户端测试工具](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/test-support/client-runtime/README.md) · [上游右栏挂载测试](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/client/ui-sidebar-right/tests/seat.client.spec.tsx#L174) · [上游右栏场景](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/apps/web/tests/shortcuts-panels.e2e.ts#L155)

复用以下基线命令。阶段 1 起将新增槽位断言纳入现有契约与桌面测试，发行验收同时保留窗口操作记录。 [现有质量门禁](code-quality.md#验收命令) · [原生试用窗口验收](creator-mode.md#对话窗口显示复验)

```powershell
node tools/check-links.mjs
node schema/check.mjs
node tools/conformance.mjs --quick
node tools/check-code-quality.mjs
tools\build-env.cmd cargo test --locked -p dshr-ui-contributions -p dshr-ui-gpui -p dshr-plugin-host
tools\build-env.cmd cargo test --locked -p dshr-desktop-gpui --bin dshr
tools\build-env.cmd cargo test --locked -p dshr-desktop-gpui --test headless_cli
tools\build-env.cmd cargo clippy --locked --workspace --all-targets -- -D warnings
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-rc5-gate.ps1
```

## 原版 React 插件试验

有明确的原版包需求时，优先用 **5–8 人日**做 Windows L4 可行性验证。选 `dsh-client-ui-cordis` 及其一个固定的浏览器视图夹具，在独立 WebView 标签页或窗口验证官方 Client Boot、React/Cordis 模块、槽位和所需 Remote。用固定输入复现一次运行、卡内挂载、交互更新和卸载，并记录来自 Rust 会话的数据、授权范围和缺失 Remote。官方的程序化 Cordis 运行卡与新创造模式的持久插件安装分别验收。 [DSH Client 模块](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/docs/subsystems/client-modules.md) · [官方 Cordis 客户端](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/extensions/ui-cordis/README.md#use-this-package) · [现有 WebView2 探针](../m1-log.md#a3-webview2--引擎已自证叠层行为待视觉确认)

只有原版客户端包用明确的 Remote 清单跑通，且可以隔离会话数据、控制窗口焦点并安全撤销时，再估算发行。按一个 Windows 发行入口、一个官方包和一个受控插件夹具暂列 **25–45 人日**，另计于可行性验证。主要投入是会话投影和客户端重连、权限与来源限制、依赖打包升级、故障回收及真窗口验收。若需要接管 Rust 会话所有权、开放通用 Host RPC，或独立窗口无法达到焦点与隔离要求，结束试验并记录缺口。完整 L5 和所有上游槽位的兼容超出本计划估算。

## 进度

- 2026-11-23 基线和估算已记录。阶段 0 待启动。试点结束后在这里记录实际人日、通过的槽位及后续预算。
- 2026-11-24 补充 ADC 目录与桌面接线的现状，确认右侧栏已有面板列表，可供试点页签复用。
- 2026-11-24 开始实施。接通 `process.ready.slots`、注册校验、撤销、试用槽位位置报告，以及 `conversation.input.left`、`conversation.input.right`、`settings.general.item` 和 `settings.section` 四个已有槽位。 [桌面槽位契约](../spec/adc-v1.md#52-桌面槽位注册)
- 2026-11-24 Windows 验收通过 38 项桌面常规测试、10 项 headless CLI 测试、82 项插件宿主单测和 24 项外部进程测试。覆盖试用实例隔离、旧注册失效、Agent 查询提供可注册的样例、真实子进程动作和更新后的视图。Schema 86/86，源码门禁和快速 conformance 通过。四个位置的发行包窗口交互尚待复验。
- 2026-11-24 右侧栏页签、中央页和主内容区状态栏接入同一注册与动作路径。目录新增 `workspace.sidebar.tab` 和 `workspace.page`，并启用原有 `status.bar.item`。通过 23 项槽位契约测试、38 项桌面测试、82 项插件宿主单测和 24 项外部进程测试。快速 conformance（Schema 88/88）、源码门禁和 workspace 严格 Clippy 通过。 [位置与生命周期契约](../spec/adc-v1.md#52-桌面槽位注册)
- 2026-11-24 Windows `--selftest-adc-panel` 使用真实子进程 fixture 通过，10 个缓存视图均构造出 GPUI 元素。该自检覆盖到元素构造，发行窗口交互见下表。
- 2026-11-24 `tool.call.toolview` 和 `tool.result.extra` 接入会话的成功工具卡。桌面和真实子进程测试覆盖插件归属、重复调用 ID、单次调用参数与结果、非法视图回退和卸载。`creator_inspect` 报告这两个位置，`creator_try_plugin` 区分注册通过与首渲染通过。 [工具槽位契约](../spec/adc-v1.md#52-桌面槽位注册) · [CPP-v1 请求](../spec/cpp-v1.md#工具视图上下文)
- 2026-11-24 工具槽位变更通过 39 项桌面测试、25 项外部进程测试、29 项 SDK 测试和 10 项 headless CLI 测试。Schema 92/92、快速 conformance、源码门禁和 workspace 严格 Clippy 通过。
- 2026-11-24 接通 `/` 与「添加」的 `command`，以及按桌面实际菜单键注册的 `menu.item`。运行层校验作用域、权限和失效注册。快捷键保留内建派发优先级，同键冲突的插件命令仍可从菜单选择。`creator_inspect` 按平台返回菜单键，试用返回注册结果。41 项桌面测试、83 项宿主单测、26 项真实子进程测试、29 项 SDK 测试和 Schema 93/93 通过。 [命令与菜单规范](../spec/adc-v1.md#52-桌面槽位注册)
- 2026-11-25 Windows 调试窗口通过 `DSHR_ADC_COMMAND_FLOW=1`，`Ctrl+Shift+G` 和从 `/fixture.refresh` 按 Enter 均触发独立的真实子进程动作并刷新视图。复验用 `--selftest-adc-panel` 在临时 `DSHR_HOME` 创建 `selftest.adc`，GUI 的 `DSHR_PROFILE_DIR` 和 `DSHR_SIDECAR_DIR` 指向该目录下 `.tmp/adc-panel-selftest` 的 `profile` 和 `sidecar`。官方 Node 侧车记录了对同名 fixture 的 JS 导入失败，Rust 宿主装载与上述动作通过。严格 Clippy 和源码门禁通过。 [窗口自检](../../apps/desktop-gpui/src/bin/chat/composer_flow.rs)
- 2026-11-25 接通 `viewer.tab`。`creator_inspect` 返回右栏文件预览和扩展名示例，试用记录注册结果。当前会话的成功交付文件可选用已登记的预览，宿主校验打开后的文件路径，再把最多 16 KiB 的文本前缀交给匹配插件。43 项桌面测试、27 项真实子进程测试、29 项 SDK 测试、23 项槽位契约测试和 10 项 headless CLI 测试通过。Schema 95/95，严格 Clippy 和源码门禁通过。 [文件视图契约](../spec/cpp-v1.md#文件视图上下文)
- 2026-11-25 Windows 调试窗口用 `DSHR_ADC_FILE_FLOW=1` 回放含 `report.TXT` 的成功交付记录。文件页签打开，真实子进程收到小写类型和预览文本，随后关闭页签并回到面板。自检沿用 `selftest.adc` 隔离配置，官方 Node 侧车的夹具导入告警仍存在。 [文件窗口自检](../../apps/desktop-gpui/src/bin/chat/view/adc_file_preview.rs)
- 2026-11-25 接通 `conversation.chat.node`。原生桌面已接通目录全部 14 个槽位，测试比较 `creator_inspect` 返回的槽位集合和 Schema 目录。节点测试覆盖同一调用 ID 的多个实例、其他插件伪造节点类型、实时回退、非法视图回退、JSONL 回放和真实子进程的发布及渲染。通过 43 项桌面测试、83 项宿主单测、28 项真实子进程测试、29 项 SDK 测试、26 项会话投影测试、23 项槽位契约测试和 Schema 97/97。 [节点规范](../spec/adc-v1.md#52-桌面槽位注册) · [作者指南](../plugin-authoring.md)
- 2026-11-25 Windows `--selftest-adc-panel` 构造 10 个真实子进程视图。使用本地回环模型夹具通过 10 项常规 headless CLI 测试，另有 10 项依赖官方包的用例列入专项门禁。源码门禁、workspace 严格 Clippy 和快速 conformance 均通过。

## 未交付项

| 范围 | 后续工作与验收条件 |
|---|---|
| `tool.call.toolview`、`tool.result.extra` | 发行窗口确认同一工具多次调用的正文及附加区。实测复制、链接确认和内建卡片回退。 |
| 右侧栏页签、导航与中央页、状态栏 | 发行窗口验收切换会话、关闭和重开页签、窄窗焦点、当前页被卸载。 |
| `viewer.tab` | 在发行窗口逐项点选预览和系统打开，复验窄窗、失效文件提示及切换会话。Linux 和 macOS 的文件句柄校验仍待实测。 |
| `conversation.chat.node` | 发行窗口复验独立节点的窄窗显示、复制与链接确认，以及实时树切换到重开会话的文本回放。 [DSH 节点组装](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/docs/subsystems/conversation.md) |
| `command`、`menu.item` | 发行包窗口回归。逐个点选「添加」「应用」「编辑」中的插件条目，并在窄窗确认焦点恢复。 |
| 原版 React 客户端 | 按独立 L4 试验取证。 |
