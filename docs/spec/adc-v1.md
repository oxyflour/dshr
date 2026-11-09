# ADC-v1 — Agent Desktop Client API

> 状态：Draft（M3 冻结） · 版本：`adc/v1` · 依赖：[`session-log.md`](session-log.md)
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
- 必填字段齐备且类型正确；
- `view` 已通过 `declareView` 声明；
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
| `conversation.chat.node` | keyed | session | `key`（node id）, `view` | none |
| `conversation.input.left` | list | session | `id, order?, label, view` | none |
| `conversation.input.right` | list | session | `id, order?, label, view` | none |
| `tool.call.toolview` | keyed | session | `key`（工具名）, `view` | none |
| `tool.result.extra` | list | session | `id, order?, label, view` | none |
| `status.bar.item` | list | root | `id, order?, label, view` | none |

Contribution 分组（用于文档与权限归类，不参与运行时分派）：`panel` → `workspace.panel`；`tool_renderer` → `tool.call.toolview`；`widget` → `conversation.chat.node`；`command`/`menu` → `command` 与 `menu.item`；`viewer` → `viewer.tab`；`settings` → `settings.section` 与 `settings.general.item`。

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

- `type` 必须是本规范列出的元素；未知元素降级为 `Placeholder`。
- `props` 必须是对象；未知属性忽略并记录诊断（不失败）。
- `children` 只允许出现在容器元素上；叶子元素带 `children` 视为契约错误。
- 限制：树深 ≤ 8、节点数 ≤ 200（单个 View 一次提交）、文本 ≤ 8 KB、单张图片 ≤ 8 MB。

### 6.2 通用属性

| 属性 | 类型 | 说明 |
|---|---|---|
| `key` | string | 同一父节点下唯一，用于 diff；缺省按位置处理 |
| `visible` | bool | 默认 true |
| `testId` | string | 测试与截图回归用 |
| `a11yLabel` | string | 可访问性名称；交互元素 MUST 提供（见 §9） |
| `tone` | enum | `neutral` / `info` / `success` / `warning` / `error` |

### 6.3 布局

| 元素 | 容器 | 主要 props |
|---|---|---|
| `Stack` | 是 | `gap`, `padding`, `align`, `justify` |
| `Row` | 是 | 同上 + `wrap` |
| `Grid` | 是 | `columns`(1–12), `gap` |
| `Divider` | 否 | `label?` |
| `Spacer` | 否 | — |

`padding`/`gap` 只接受 `0/2/4/8/12/16/24/32`；`align` ∈ `start/center/end/stretch`；`justify` ∈ `start/center/end/space-between`。

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

### 6.7 媒体

| 元素 | props |
|---|---|
| `Image` | `attachmentId`（内容寻址引用，禁止 URL 直取）, `alt`, `fit` |
| `Icon` | `name`（宿主图标集内的名称） |
| `Chart` | `kind`(`bars`/`line`/`donut`), `data[]{label,value}`, `series?` |

### 6.8 容器

| 元素 | props |
|---|---|
| `Card` | `title?`, `tone?`, `children` |
| `Tabs` | `tabs[]{key,label}`, `activeKey`, `onChange` |
| `Accordion` | `sections[]{key,title,children}`, `expanded[]` |

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

- `declareView` 必须在 `register` 之前；未声明的 `view` 会被拒绝。
- View 卸载时其本地状态（滚动位置、展开项）可保留在 client-local state，但不进入 Session Log。
- 插件重载后 `view` 需重新声明；Host 不缓存旧的元素树。

## 9. 版本与兼容

| 变更 | 级别 | 要求 |
|---|---|---|
| 新增元素/属性/新 slot | minor | 插件用能力位探测：`ctx.capability('adc.element.Chart')` |
| 修改已有元素语义 | major | 旧元素集保留至少一个 major 周期 |
| 删除元素 | major | 先标记 `deprecated`，再删除 |

未知元素渲染为 `Placeholder`（带 `testId` 与诊断），MUST NOT 抛错或整棵树丢弃。

可访问性（M2 起强制）：所有交互元素必须有 `a11yLabel`；焦点顺序按文档序；键盘可达 `Tab`/`Enter`/`Space`/`Esc`。

## 10. 安全与配额

1. 元素树不得触发网络请求；图片只能引用内容寻址附件。
2. 不允许任意样式、任意 HTML、任意脚本注入。
3. 单次提交配额：8 层深度、200 节点、8 KB 文本、256 条 patch。
4. 插件只能读取自己声明过的绑定前缀；跨插件读取一律拒绝。
5. WebView 容器元素不在 v1 范围；需要 DOM 的插件走独立 Panel，由 Host 单独授权。

## 11. 校验与工具链

- JSON Schema：[`../../schema/adc-v1/element.schema.json`](../../schema/adc-v1/element.schema.json)、[`slot-registration.schema.json`](../../schema/adc-v1/slot-registration.schema.json)。
- 规范检查（lint）必须覆盖：未知元素、叶子带 children、交互元素缺 `a11yLabel`、超配额、非法动作、非 https 外链。
- Conformance 前缀：`ADC-SLOT-*`（注册/撤销/冲突）、`ADC-ELEM-*`（词汇表/降级）、`ADC-BIND-*`（绑定/更新）、`ADC-SEC-*`（配额/权限）。

## 12. 未决问题

| # | 问题 | 倾向 | 决定时点 |
|---|---|---|---|
| Q1 | Markdown 子集的确切范围 | 只支持标题/列表/代码/强调/链接/表格 | M2 |
| Q2 | `Chart` 的数据上限与配色规则 | ≤ 8 数据点，配色仅用语义色 | M2 |
| Q3 | 是否允许自定义 `Grid` 跨列 | 允许 `span`，最大 12 | M2 |
| Q4 | 无障碍契约的完整清单 | 先按 §9 四条，M2 补全 | M2 |
| Q5 | WebView 容器元素是否进入 v2 | 需要独立 Panel 生命周期设计 | M4 后 |
