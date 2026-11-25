# ADC-v1 — Agent Desktop Client API

> 状态：Frozen（M3 冻结，ADR-0011） · 版本：`adc/v1` · 冻结工件：[`schema/adc-v1/slots.v1.json`](../../schema/adc-v1/slots.v1.json) · 依赖：[`session-log.md`](session-log.md)
> 适用对象：所有希望向 GPUI Shell 提交 UI 的插件——原生 Rust、脚本（QuickJS）、隔离 WebView。

---

## 1. 范围

本规范定义三件事：

1. **插件把内容挂到哪里**：slot 注册表。
2. **插件能画什么**：声明式元素词汇表（JSON 树）。
3. **插件怎么与宿主交互**：只读绑定、动作、事件。

不在范围：宿主自身的布局算法、主题色值、动画曲线、渲染器实现（GPUI 内部细节一律不暴露）。

## 2. 设计原则

| # | 原则 | 含义 |
|---|---|---|
| P1 | 数据不是代码 | 元素是 `{type, props, children}` 纯 JSON；不传函数、不传对象引用、不传 GPUI 类型 |
| P2 | 无 CSS、无自由布局 | 只有 `Stack/Row/Grid` 与主题 token；插件无法改变宿主整体几何 |
| P3 | 组合优于自由 | 宁可多给预置 slot，也不给"改整棵界面树"的能力 |
| P4 | 前向兼容 | 未知元素/属性必须降级渲染并诊断，不得崩溃 |
| P5 | Host 拥有状态 | 插件只读快照、提交动作；不持有 Session/Agent 对象 |

## 3. 术语

- **Host**：Rust Client Host（`ui-contributions`），持有 slot 注册表与状态快照。
- **View**：一个具名渲染单元，输入是绑定状态，输出是元素树。
- **Entry**：一次 slot 注册，由 `registration_id` 标识。
- **Contribution**：宿主预置的一组 slot 的语义分组（panel / tool_renderer / …），见 §5。

## 4. Slot 模型

### 4.1 声明

每个 slot 由宿主声明，包含：`slot_id`、`kind`、`scope`、注册契约（必填字段与校验规则）、替换风险（`none` / `shadows-shipped-ui`）、以及可选的 key 域（闭合枚举或开放）。

插件**不能**声明新 slot。需要新挂载点时，向宿主提需求；宿主新增 slot 属于 minor 变更。

### 4.2 kind 语义

| kind | 语义 | 冲突解决 |
|---|---|---|
| `single` | 单席 | 后注册者替换前者；若该 slot 标记 `exclusive`，改为拒绝并诊断 |
| `list` | 有序列表 | 按 `order` 升序，`order` 相同按 `registration_id` 升序；`id` 重复即拒绝 |
| `keyed` | 按 key 分派 | key 域闭合时非法 key 拒绝；同一 key 后注册者替换（`single` 语义） |
| `chain` | 链式 | 按 `order` 形成管道，每个处理器可返回值、返回 `null` 表示"交给下一个"、抛错表示"短路为默认值" |

### 4.3 scope

| scope | 生命周期 |
|---|---|
| `root` | 应用级，全局一份 |
| `session` | 每个 Session 一份；会话关闭即销毁 |
| `session-maybe` | 同 `session`，但允许"尚未创建 Session"的空态存在 |

### 4.4 注册与撤销

```ts
const off = ctx.ui.register('workspace.panel', {
  id: 'cad.inspector',
  order: 100,
  label: 'CAD Inspector',
  location: 'right',
  view: 'cad.inspector',
})
// off() 幂等；插件卸载时由 Host 自动调用
```

必须满足：

1. 注册绑定到当前 `generation`；`generation` 失效后所有注册自动撤销。
2. 撤销必须幂等；重复撤销不报错。
3. 撤销顺序为**逆注册序**（后注册先撤销）。
4. 卸载超时（默认 5s）后强制撤销并记录诊断，不阻塞其他插件。
5. 注册失败必须给出可读诊断：未知 slot、契约不符、重复 id、权限不足。

### 4.5 校验

Host 在注册时校验，任一失败即拒绝并上报 `adc.registration.rejected`：

- slot 存在且插件有权限注册；
- 必填字段齐备且类型正确（字段集与取值以 §5.1 的 slot 目录为准；`command`/`menu.item` 用 `run` 而不是 `view`）；
- 契约含 `view` 时，`view` 已在 `declareView` 中声明；
- `id`/`key` 在作用域内唯一（`single`/`keyed` 的覆盖是允许的，需记录告警）。

## 5. Slot 目录 v1

slot 全集由 Host 维护；下表是 v1 的首批集合，后续只增不改语义。

| slot_id | kind | scope | 注册契约 | 替换风险 |
|---|---|---|---|---|
| `workspace.panel` | list | root | `id, order?, label, location, view` | none |
| `viewer.tab` | keyed | session | `key`（文件类型）, `view` | none |
| `settings.section` | list | root | `id, order?, label, view` | none |
| `settings.general.item` | list | root | `id, order?, label, view` | none |
| `command` | list | root | `id, label, keys?, run` | none |
| `menu.item` | keyed | root | `key`（菜单名）, `id, order?, label, run` | none |
| `conversation.chat.node` | keyed | session | `key`（节点类型）, `view` | none |
| `conversation.input.left` | list | session | `id, order?, label, view` | none |
| `conversation.input.right` | list | session | `id, order?, label, view` | none |
| `tool.call.toolview` | keyed | session | `key`（工具名）, `view` | none |
| `tool.result.extra` | list | session | `id, order?, label, view` | none |
| `status.bar.item` | list | root | `id, order?, label, view` | none |
| `workspace.sidebar.tab` | list | session | `id, order?, label, view` | none |
| `workspace.page` | list | root | `id, order?, label, view` | none |

Contribution 分组（用于文档与权限归类，不参与运行时分派）：`panel` → `workspace.panel`、`workspace.sidebar.tab` 与 `workspace.page`；`tool_renderer` → `tool.call.toolview`；`widget` → `conversation.chat.node`；`command`/`menu` → `command` 与 `menu.item`；`viewer` → `viewer.tab`；`settings` → `settings.section` 与 `settings.general.item`；`input` → `conversation.input.left`/`conversation.input.right`；`tool_extra` → `tool.result.extra`；`status` → `status.bar.item`。

### 5.1 目录 JSON 是唯一事实源

机器可读的 slot 目录是 [`schema/adc-v1/slots.v1.json`](../../schema/adc-v1/slots.v1.json)；本节表格是它的人读镜像，二者必须逐项一致（`node schema/check.mjs` 同时校验目录自检与注册契约）。

目录每项字段：

| 字段 | 含义 |
|---|---|
| `slot_id` | 点分小写标识（`command` 为 v1 保留的历史短名）；规则见 [`README.md`](README.md) §3 |
| `kind` | `single` / `list` / `keyed` / `chain`，语义见 §4.2 |
| `scope` | `root` / `session` / `session-maybe`，语义见 §4.3 |
| `exclusive` | 仅影响覆盖语义：`true` 时拒绝后注册者；目录现有槽位均为 `false` |
| `replacement_risk` | `none` / `shadows-shipped-ui`；v1 全部为 `none` |
| `contribution` | 文档与权限分组，取值见本节末段 |
| `registration` | 注册契约：`required` / `optional` 字段名；字段类型与取值范围见目录的 `field_types` |
| `key_domain` | 仅 keyed：`{"mode":"open"}` 或 `{"mode":"closed","source":"host"}` |

注册校验分两层，缺一不可：① [`slot-registration.schema.json`](../../schema/adc-v1/slot-registration.schema.json) 做 kind 级载荷形状校验（view 型必须带 `view`；run 型必须带 `run`，如 `command`/`menu.item`；keyed 必须带 `key`）；② Host 按 `slot_id` 查目录，按 `registration` 逐字段校验并拒绝未知字段。schema 不知道 `slot_id`，所以字段级必填（如 `workspace.panel` 的 `label`/`location`）只由目录表达。

v1 只注册 `list` 与 `keyed`；`single`/`chain` 保留 kind 语义但目录中没有条目，新增属于 minor 变更（先改目录与本节表格）。keyed slot 的 key 域：`viewer.tab`（文件类型）、`conversation.chat.node`（节点类型）、`tool.call.toolview`（工具名）为 open——值域由对应的插件/宿主注册表决定；`menu.item`（菜单名）为 closed，值域由 Host 菜单目录声明，非法 key 由 Host 拒绝。

### 5.2 桌面槽位注册

CPP-v1 外部进程通过 `process.ready.slots` 声明输入和设置贡献。`conversation.input.left`、`conversation.input.right` 分别挂在输入卡工具行左侧和发送按钮前。`settings.general.item` 挂在「设置 → 常规」。`settings.section` 在设置导航加一个独立页面，选中的贡献卸载或不可渲染时回到常规。工作区面板沿用 `panels`。未知或桌面尚未接通的 `slot_id` 使声明失败。 [CPP-v1 声明](cpp-v1.md#123-声明processready)

宿主同时校验包身份、目录字段和同一注册表内的 ID 冲突。列表按 `order`、`id` 排序。安装的贡献在当前桌面会话可见。创造模式试用贡献仅在所属会话可见。卸载和成功替换会撤销旧注册，旧控件的动作以注册身份检查后派发。

视图复用 `ui.render` 和 `ui.action`。桌面渲染读取快照，树无效或插件取树失败时跳过该视图，输入和设置的内建控件继续可用。生成中显示已有快照。试用逐一校验视图，并返回通过校验的槽位及显示位置。`creator_inspect` 提供实际接通的位置和注册示例。

`workspace.sidebar.tab` 在聊天右侧栏添加独立页签，使用声明的 `label`。右侧栏默认打开既有的面板列表。页签可以关闭并重新打开，关停贡献或切换会话时回到面板列表。已关闭页签只影响当前会话的显示。文件预览保留 `viewer.tab` 的文件类型语义。

`viewer.tab.key` 是小写 ASCII 文件扩展名，规则为 `^[a-z0-9][a-z0-9_+-]{0,31}$`。桌面按交付记录中文件名的最后一个扩展名查找注册，大小写归一，优先使用当前会话的试用注册。同一注册表使用 keyed 替换语义。匹配时，成功交付的文件获得「预览」入口，打开右栏的文件页签。新预览替换旧文件页签，关闭页签、切换会话或撤销贡献释放预览资源。「系统打开」仍可使用。

宿主在用户明确选择后重新检查文件及其已打开句柄的位置，且只向匹配插件发送 [文件视图上下文](cpp-v1.md#文件视图上下文)。工作区外、丢失、文件类型不符和异常句柄不读取内容。渲染失败时右栏显示错误，并保留系统打开入口。预览树使用 `copy` 和受确认保护的 `openExternal`，预览本身不写入会话日志。Agent 试用回报文件类型和注册状态，选择匹配文件后才验证渲染。

`workspace.page` 用一项注册同时添加左侧导航入口和中央页面，用户通过「返回会话」或选择会话回到对话。入口和页面随同一注册撤销或失效。试用页的可见性随所属会话，安装的页在各桌面会话可选择。

`status.bar.item` 显示在主内容区底部的状态栏。无有效贡献时不占用高度。可交互的状态项继续通过当前注册派发动作。

`tool.call.toolview` 用注册的 `key` 匹配同一插件声明的工具名，替换对应成功调用的卡片正文。`tool.result.extra` 在同一插件的成功工具卡下方追加内容，沿用列表顺序。工具卡保留名称、状态和交付文件入口，旧版 `meta.ui` 仍可展示。两种注册只接收所属插件的会话内调用，使用 [工具视图上下文](cpp-v1.md#工具视图上下文)。试用返回注册校验结果，调用成功时才渲染对应视图。

宿主在桌面工作线程取得并校验工具视图，当前会话保留最近 16 个工具视图快照。渲染失败、树无效、插件撤销、重开日志或快照退出缓存时，使用内建工具卡和已有结果。调用错误和取消保留内建状态。工具卡动作只支持 `copy` 和 `openExternal`，沿用宿主处理及 URL 确认。

`conversation.chat.node` 在对话流中渲染由同一插件发布的独立节点。`key` 是可复用的类型，格式为 1–64 位 ASCII 字母、数字或 `._-`。同一个宿主中，每个节点类型归一个插件注册。同一插件可以更新自己的注册。插件通过成功工具结果的 `meta.adc_node` 发布一个节点。宿主用该工具的 `tool.call` 事件序号区分实例，并按工具结算顺序投影。节点只读取自己的数据，支持 `copy` 与受宿主确认的 `openExternal`。

有效节点在工具结算后即显示回退文本。当前轮取得正确的 ADC 树后用视图替换回退。历史会话直接回放文本。未知类型、失败调用或畸形声明不生成节点。视图无效、超时、过期或插件卸载时恢复文本。试用报告注册状态，实际调用插件工具后校验渲染。 [节点视图上下文](cpp-v1.md#对话节点上下文)

`command` 在输入框的 `/` 命令菜单和「添加」菜单公开 `/<id>`。内建命令 ID 保留。`keys` 可声明单次组合键，用 `+` 或 `-` 连接修饰键与按键。至少包含 `Ctrl` 或 `Cmd`（`Meta`、`Super` 同义），可再加 `Alt`、`Shift`。按键支持 ASCII 字母、数字、`F1` 至 `F24`、`Enter`、`Esc`、`Space`、`Tab`、编辑键和方向键。内建快捷键优先。多个可见插件命令登记同一组合键时该键不派发，命令仍可从菜单执行。

`menu.item` 的桌面菜单键为 `composer.add`，Windows 另提供 `window.application` 和 `window.edit`。每个键保留一条贡献，按现有 keyed 替换语义注册。插件条目追加在内建项之后。未知键使声明失败，实际菜单键由 `creator_inspect` 返回。

两类动作只对当前可见注册派发，试用只在所属会话生效。试用注册与安装注册使用同一 ID 或菜单键时，当前会话优先使用试用注册。只读模式和生成中拒绝派发。ADC `{ "command": "<id>" }` 动作只解析当前视图所属插件实例的有效命令。动作使用 CPP-v1 的 `ui.action`，由 `run` 定位插件处理器。 [命令派发](cpp-v1.md#桌面命令派发)

## 6. 元素词汇表

### 6.1 节点结构

```json
{
  "type": "Stack",
  "key": "root",
  "props": { "gap": 8 },
  "children": [
    { "type": "Text", "props": { "value": "Hello" } },
    { "type": "Button", "props": { "label": "运行", "onPress": { "call": "cad.validate" } } }
  ]
}
```

规则：

- `type` 必须是本规范列出的元素；未知元素在作者期 lint（schema）判 invalid，运行时降级为 `Placeholder`。
- `props` 必须是对象；未知属性在运行时忽略并记录诊断（不失败），作者期 lint 对未知元素字段判 invalid。
- `children` 只允许出现在 `Stack`/`Row`/`Grid`/`Card`/`Tabs` 上；`Card` 必须带 `children`；`Accordion` 的内容在 `sections[].children`，不使用节点级 `children`；叶子元素带 `children` 视为契约错误。
- 限制：树深 ≤ 8、节点数 ≤ 200（单个 View 一次提交）、文本 ≤ 8 KB、单张图片 ≤ 8 MB。
- 字段必填性与取值范围以 [`schema/adc-v1/element.schema.json`](../../schema/adc-v1/element.schema.json) 为准，本节只定义命名与语义。

作者期 lint 与运行时是两件事：schema 对未知元素、未知节点字段、未知元素字段、叶子带 `children` 判 invalid（严格）；运行时按 P4 把未知元素降级为 `Placeholder`、忽略未知字段并诊断（宽松，不失败）。`a11yLabel` 在作者期可选（schema 不强制），运行时对交互元素 MUST 提供（见 §9，`ADC-ELEM-A11Y-001`）。

### 6.2 通用属性

| 属性 | 类型 | 说明 |
|---|---|---|
| `key` | string | 节点级；同一父节点下唯一，用于 diff；缺省按位置处理 |
| `visible` | bool | 默认 true |
| `testId` | string | 测试与截图回归用 |
| `a11yLabel` | string | 可访问性名称；交互元素 MUST 提供（见 §9）；作者期可选，运行时强制 |
| `tone` | enum | `neutral` / `info` / `success` / `warning` / `error` |
| `span` | integer | `Grid` 子项的跨列数（1–12）；非 Grid 子项忽略 |

`tone` 只使用主题 token 的语义色，插件不能指定色值。

### 6.3 布局

| 元素 | 容器 | 主要 props |
|---|---|---|
| `Stack` | 是 | `gap`, `padding`, `align`, `justify` |
| `Row` | 是 | 同上 + `wrap` |
| `Grid` | 是 | `columns`(1–12), `gap` |
| `Divider` | 否 | `label?` |
| `Spacer` | 否 | — |

`padding`/`gap` 只接受 `0/2/4/8/12/16/24/32`；`align` ∈ `start/center/end/stretch`；`justify` ∈ `start/center/end/space-between`。`Grid.columns` 必填且为 1–12；Grid 子项可用 §6.2 的 `span` 跨列（最大 12）。

### 6.4 文本

| 元素 | props |
|---|---|
| `Text` | `value`, `style`(`body`/`caption`/`title`/`mono`), `tone`, `maxLines` |
| `Markdown` | `source`（受控子集：标题/列表/代码/强调/链接/表格）, `maxLines` |
| `Code` | `source`, `language`, `lineNumbers` |
| `Badge` | `label`, `tone` |

### 6.5 交互

| 元素 | props |
|---|---|
| `Button` | `label`, `tone`, `disabled`, `onPress` |
| `Input` | `value`, `placeholder`, `multiline`, `onChange` |
| `Textarea` | `value`, `rows`, `onChange` |
| `Select` | `value`, `options[]{value,label}`, `onChange` |
| `Checkbox` | `checked`, `label`, `onChange` |
| `Switch` | `checked`, `label`, `onChange` |
| `Slider` | `value`, `min`, `max`, `step`, `onChange` |

### 6.6 数据

| 元素 | props |
|---|---|
| `List` | `items[]{key,primary,secondary?}`, `onSelect?` |
| `Table` | `columns[]{key,title,width?}`, `rows[]`, `onRowClick?` |
| `Tree` | `nodes[]{key,label,children?}`, `expanded[]`, `onToggle?` |
| `KeyValue` | `pairs[]{key,value}` |

`Table.rows` 是对象数组：键为列 `key`，单元格取值 `string`/`number`/`boolean`/`null`；`columns` 至少 1 列。`Tree.nodes` 可递归嵌套 `children`。

### 6.7 媒体

| 元素 | props |
|---|---|
| `Image` | `attachmentId`（内容寻址引用，禁止 URL 直取）, `alt`, `fit` |
| `Icon` | `name`（宿主图标集内的名称） |
| `Chart` | `kind`(`bars`/`line`/`donut`), `data[]{label,value}`, `series?` |

`Image.fit` 取值 `cover`/`contain`/`fill`；`attachmentId` 必须是内容寻址附件 ID（不以 URL 形式给值）。`Chart` 单图 ≤ 8 个数据点；可选 `series` 为 `{label, values[]}`，最多 4 条、每条 ≤ 8 点（见 §12 Q2）。

### 6.8 容器

| 元素 | props |
|---|---|
| `Card` | `title?`, `tone?`, `children` |
| `Tabs` | `tabs[]{key,label}`, `activeKey`, `onChange` |
| `Accordion` | `sections[]{key,title,children}`, `expanded[]` |

`Card` 必须带节点级 `children`；`Tabs` 的节点级 `children` 是当前激活页内容；`Accordion` 的内容位于 `sections[].children`。

## 7. 绑定、动作与事件

### 7.1 绑定

`ctx.binding(path)` 返回只读快照；可用前缀（v1）：

| 前缀 | 内容 | 权限 |
|---|---|---|
| `session.` | 当前会话摘要（标题、状态、token 占用、turn/step） | 需 `session:read` |
| `workspace.` | 工作区路径、文件列表摘要 | 需 `workspace:read` |
| `tool.` | 当前工具调用的参数/结果摘要 | 需 `tool:read` |
| `plugin.` | 本插件的配置与私有状态 | 默认允许 |

未授权路径返回 `null` 并记录诊断，不抛错。

### 7.2 更新协议

Host 向 View 推送 `{ seq, snapshot?, patches[] }`；`patches` 为有序 `{op: "set"|"remove", path, value?}`。规则：

- `seq` 单调；插件收到乱序直接丢弃。
- 单次 patch 集合 ≤ 256 条；超出时 Host 改为 full snapshot。
- 插件 MUST 把渲染视为纯函数：同一 `seq` + 同一树必须得到同一结果。

### 7.3 动作

| 动作 | 形态 | 约束 |
|---|---|---|
| `call` | `{ "call": "<method>", "args": {...} }` | 只能调用插件自己的 Host 方法 |
| `command` | `{ "command": "<id>" }` | 只能触发已注册的 command |
| `openExternal` | `{ "openExternal": "https://..." }` | 仅 https，需用户确认，禁止 `file:`/`javascript:` |
| `copy` | `{ "copy": "text" }` | 长度 ≤ 64 KB |

### 7.4 事件

只允许三个回调：`onPress`、`onChange`、`onSubmit`。

- `onChange` 由 Host 去抖（默认 150ms），插件不需要自己节流。
- 回调只产生动作，不能同步返回 DOM 或修改状态；状态变化必须走 Host。

## 8. 生命周期

```text
declareView → register(slot) → mount → [patch…] → unmount → unregister → dispose
```

- `declareView` 必须在 `register` 之前（仅契约含 `view` 的注册）；未声明的 `view` 会被拒绝。
- View 卸载时其本地状态（滚动位置、展开项）可保留在 client-local state，但不进入 Session Log。
- 插件重载后 `view` 需重新声明；Host 不缓存旧的元素树。

### 8.1 插件持久化状态

三类状态严格区分（与 [`PLAN.md`](../PLAN.md) §4.4 一致）：

| 状态 | 归属 | 是否持久化 | 例 |
|---|---|---|---|
| Host-owned state | Host（Rust 权威） | 由 Host 决定 | Session、Tool Call、审批结果、文件身份 |
| Client-local state | 客户端 UI | 否，可随时丢弃，不进 Session Log | 面板尺寸/位置、焦点、选中项、折叠状态、未提交输入 |
| Plugin state | 插件声明、Host 代写 | 是 | 插件配置、上次选择、缓存索引 |

插件持久化状态契约：

1. **声明**：插件在 manifest（`cpp-v1`）或 `apply` 时声明 `state.schemaVersion`（整数，从 1 起）与需要持久化的字段；Host 只接受声明过的字段，未声明字段拒绝并诊断。
2. **读写**：只能通过 Host 提供的 `ctx.state.get(key)` / `ctx.state.set(key, value)`；值为 JSON（禁止 `NaN`/`Infinity`、函数、对象引用），单键 ≤ 8 KB、每插件合计 ≤ 64 KB；超限拒绝并诊断，读回最后一次已提交值。
3. **schema 版本与迁移钩子**：Host 持久化 `{schemaVersion, data}`。加载时若 `schemaVersion` 低于当前版本，Host MUST 调用插件声明的纯函数 `migrate(fromVersion, data) -> data`（同步、≤ 1 s、不得访问网络/Session/文件）；迁移只能升版本；迁移失败或超时 → 丢弃该状态并用默认值启动，同时把原数据留在隔离区（quarantine）供人工排查，写诊断 `ADC-STATE-MIGRATE-001`，不得阻塞插件加载。
4. **与 client-local state 的边界**：滚动位置、焦点、展开项等渲染状态 MUST NOT 进入 plugin state 或 Session Log（见上表）；需要跨会话恢复的"业务性"状态（例如上次选择的文件）走 plugin state。
5. **隔离**：状态以 `plugin_id` 为命名空间，跨插件读取一律拒绝；卸载插件时状态保留（重装可恢复），用户显式"清除插件数据"时删除；插件不得读写 Session Log 或其它插件的视图状态。
6. **并发**：同一插件同一时刻只有一个写入者（Host 在 `apply`/视图挂载期间串行化写入）；`set` 返回前不得假设已落盘。

## 9. 版本与兼容

| 变更 | 级别 | 要求 |
|---|---|---|
| 新增元素/属性/新 slot | minor | 插件用能力位探测：`ctx.capability('adc.element.Chart')` |
| 修改已有元素语义 | major | 旧元素集保留至少一个 major 周期 |
| 删除元素 | major | 先标记 `deprecated`，再删除 |

未知元素渲染为 `Placeholder`（带 `testId` 与诊断），MUST NOT 抛错或整棵树丢弃。

可访问性（M2 起强制）：所有交互元素必须有 `a11yLabel`（作者期 schema 不强制、运行时 MUST 拒绝，`ADC-ELEM-A11Y-001`）；焦点顺序按文档序；键盘可达 `Tab`/`Enter`/`Space`/`Esc`。完整清单见 §12 Q4。

## 10. 安全与配额

1. 元素树不得触发网络请求；图片只能引用内容寻址附件。
2. 不允许任意样式、任意 HTML、任意脚本注入。
3. 单次提交配额：8 层深度、200 节点、8 KB 文本、256 条 patch。
4. 插件只能读取自己声明过的绑定前缀；跨插件读取一律拒绝。
5. WebView 容器元素不在 v1 范围；需要 DOM 的插件走独立 Panel，由 Host 单独授权。

## 11. 校验与工具链

- 冻结工件：[`schema/adc-v1/slots.v1.json`](../../schema/adc-v1/slots.v1.json)（slot 目录，唯一事实源，数据非 JSON Schema）、[`element.schema.json`](../../schema/adc-v1/element.schema.json)（26 元素字段级 props）、[`slot-registration.schema.json`](../../schema/adc-v1/slot-registration.schema.json)（kind 级载荷形状）。
- 作者期 lint（`node schema/check.mjs`）严格：未知元素、未知节点字段、未知元素字段、叶子带 `children`、超配额、非法动作、非 https 外链判 invalid；未知字段在运行时按 P4 忽略并诊断——两件事分开，见 [`review-m3-plan.md`](../review-m3-plan.md) §10 第 9 条。
- 运行时拒绝（不写进 schema）：交互元素缺 `a11yLabel`（`ADC-ELEM-A11Y-001`）、未知元素降级为 `Placeholder`（`ADC-ELEM-*`）、插件状态迁移失败（`ADC-STATE-MIGRATE-001`）、注册未知 slot/契约不符（`ADC-SLOT-*`）。
- Conformance 前缀：`ADC-SLOT-*`（注册/撤销/冲突）、`ADC-ELEM-*`（词汇表/降级/a11y）、`ADC-BIND-*`（绑定/更新）、`ADC-SEC-*`（配额/权限）、`ADC-STATE-*`（插件持久化状态）。

## 12. 未决问题（v1 已关闭）

| # | 问题 | 决定 | 理由 | 关闭时点 |
|---|---|---|---|---|
| Q1 | Markdown 子集的确切范围 | 只支持标题/列表/代码/强调/链接/表格；不执行内联 HTML/脚本，未识别语法按纯文本渲染 | 禁任意 HTML/脚本是 §10 硬约束；子集已覆盖会话展示的主场景 | M3（Review） |
| Q2 | `Chart` 的数据上限与配色规则 | 单图 ≤ 8 个数据点；可选 `series` 最多 4 条、每条 ≤ 8 点；配色只用主题 token 的语义色（`tone`），插件不能指定色值 | 渲染预算（M1 A1c 单行 4 元素 draw p50 15 ms）与主题一致性；8 点是柱/折/环图的可读上限 | M3（Review） |
| Q3 | 是否允许自定义 `Grid` 跨列 | 允许：元素通用属性 `span`（1–12），仅 Grid 子项生效，超过 12 由 Host 按 12 截断并诊断 | 常见表单布局需要跨列；仍不引入自由布局（P2） | M3（Review） |
| Q4 | 无障碍契约的完整清单 | v1 按 §9 四条：交互元素必须有 `a11yLabel`（运行时拒绝）、焦点顺序=文档序、键盘 `Tab`/`Enter`/`Space`/`Esc`、未知元素 Placeholder 带 `testId`+诊断；另加 Image `alt`、颜色不得单独承载语义、焦点环用主题 token | 完整 WCAG 2.2 AA 审计需专门预算；v1 先冻结可机检的强制项，M4 做审计补全 | M3（Review） |
| Q5 | WebView 容器元素是否进入 v2 | 明确不在 v1：元素词汇表不含 WebView/DOM/HTML；需要 DOM 的插件走独立 Panel（Host 单独授权，§4.2 C），v2 也不新增元素级 WebView；重新评估时点为 M4 之后的 Panel 生命周期评审 | 元素级 WebView 与隔离/配额/焦点模型冲突；M1 A3 证明 GPUI 浮层不能可靠覆盖 WebView（[`m1-log.md`](../m1-log.md) L131–152） | M3（Review），v2 保持排除 |
