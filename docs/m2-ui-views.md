# M2 原生视图：Diff / Terminal（2026-11-11）

W5 交付：在 `crates/ui-gpui` 里补齐 Diff 与 Terminal 两个原生 GPUI 组件。
解析是纯函数（无窗口可测），渲染只做 token → 样式映射；不引入新依赖，不碰
`apps/desktop-gpui/**`（接线由 Lead 在集成轮完成）。

## 文件与 API

| 文件 | 内容 |
|---|---|
| [`crates/ui-gpui/src/diff.rs`](../crates/ui-gpui/src/diff.rs) | unified diff 解析 + `diff_view` |
| [`crates/ui-gpui/src/terminal.rs`](../crates/ui-gpui/src/terminal.rs) | ANSI 终端解析 + `terminal_view` |
| [`crates/ui-gpui/src/lib.rs`](../crates/ui-gpui/src/lib.rs) | `mod diff/terminal` + re-export（只加这两块） |

### Diff

```rust
pub fn parse_diff(input: &str) -> Diff;
pub fn diff_view(theme: &Theme, diff: &Diff) -> AnyElement;      // 主入口
pub fn diff_view_from_text(theme: &Theme, text: &str) -> AnyElement; // 便捷入口
pub const MAX_RENDER_LINES: usize = 2000;  // 单次渲染上限（模型仍保存全部）
```

模型：

```rust
pub struct Diff {
    pub files: Vec<DiffFile>,
    pub notices: Vec<String>,          // 无法归到文件的行（combined diff、未识别头部）
}
pub struct DiffFile {
    pub old_path: String,              // `---` 行（/dev/null 表示新增）
    pub new_path: String,              // `+++` 行
    pub is_binary: bool,
    pub metadata: Vec<String>,         // index / new file mode / rename from …
    pub hunks: Vec<DiffHunk>,
}
pub struct DiffHunk {
    pub old_start: u32, pub old_count: u32,
    pub new_start: u32, pub new_count: u32,
    pub section: String,               // `@@` 之后的函数签名等
    pub lines: Vec<DiffLine>,
}
pub struct DiffLine {
    pub kind: DiffLineKind,            // Context / Add / Remove
    pub old_line: Option<u32>,         // 删除行只有旧行号
    pub new_line: Option<u32>,         // 新增行只有新行号
    pub text: String,                  // 去掉行首标记后的正文
    pub no_newline: bool,              // 下一行是 `\ No newline at end of file`
}
```

解析规则：

- 文件边界：`diff --git a/x b/y`；也接受不带 `diff --git` 的裸 `---`/`+++` 补丁。
- hunk 头：`@@ -old[,count] +new[,count] @@ section`，count 省略按 1；
  行号按 hunk 头逐行推进，`+`/`-`/空格行分别写新号、旧号或两边。
- `\ No newline at end of file` 不生成独立行，而是给上一行置 `no_newline`，渲染时在
  该行下方补一条灰色说明（不影响行号）。
- 二进制：`Binary files a/x and b/x differ` 与 `GIT binary patch` 置 `is_binary`，
  后者跳过 base85 负载；视图只显示"二进制文件，不显示内容"。
- 空 diff：`Diff::is_empty()` 为真，视图显示"（无差异）"。
- 缺失/畸形头部不 panic：未知行进 `notices`，文件级元信息进 `metadata`；
  combined diff（`diff --cc` / `@@@`）明确记为 notice 而不是吞掉正文。
- 行尾 CRLF/CR 统一按 LF 处理（解析前归一化）。
- 路径：git 的 `a/` `b/` 前缀在展示时去掉；`/dev/null` 表示新增/删除；
  旧新路径不同时展示 `old → new`。

渲染：

- 等宽字体 `theme.font_code`，`small`/`small_line` 字号行高；
- 文件头（路径 + `+N`/`−M` + 二进制徽标）用 `code_block_banner` 底；
- hunk 头用 `tag` 底、`label_secondary` 文字；
- 新增/删除行：底色 `state_success` / `state_error` 的 10% 透明度，行号与 `+`/`-`
  标记用对应的语义色；上下文行用 `label_caption` 行号；
- 每行两列行号（旧/新，右对齐）+ 标记列 + 正文；
- 容器：`code_block` 底、`border_l2` 边、圆角 `radii.md`，最大高度 420px，
  纵向 + 横向滚动（超长行可横向滚动，内容最小宽度按字符数估算）；
- 单次渲染最多 `MAX_RENDER_LINES`（2000）行，超出以"diff 过大，仅渲染前 N 行"收尾；
  模型始终保存完整 diff，滚动位置用内容指纹做元素 id。

### Terminal

```rust
pub fn parse_terminal(input: &str) -> Terminal;
pub fn terminal_view(theme: &Theme, lines: &[TermLine]) -> AnyElement;    // 主入口
pub fn terminal_view_from_text(theme: &Theme, text: &str) -> AnyElement;  // 便捷入口
pub const MAX_LINES: usize = 5000;
```

模型：

```rust
pub struct Terminal {
    pub lines: Vec<TermLine>,          // 已按 CR/LF/光标操作结算后的屏幕行
    pub truncated: bool,               // 超过 MAX_LINES，最早的若干行被丢弃
    pub unsupported: usize,            // 不支持/畸形控制序列计数（诊断用）
}
pub struct TermLine { pub runs: Vec<TermRun> }
pub struct TermRun  { pub text: String, pub style: TermStyle }
pub struct TermStyle {
    pub fg: Option<AnsiColor>, pub bg: Option<AnsiColor>,
    pub bold: bool, pub dim: bool, pub italic: bool,
    pub underline: bool, pub reverse: bool,
}
pub enum AnsiColor { Indexed(u8), Rgb { r: u8, g: u8, b: u8 } }
```

解析规则：

- 文本控制：`\r` 回到行首；`\n` 换行并回到行首（模拟 tty 的 ONLCR，`cat` 输出的
  `\n` 不会出现阶梯缩进）；`\x08` 左移一格（不擦除）；`\t` 展开到 8 列制表位。
- SGR：`0/1/2/3/4/7/22/23/24/27`、`30-37/39`、`40-47/49`、`90-97`、`100-107`；
  扩展色 `38;5;n`、`48;5;n`、`38;2;r;g;b`、`48;2;r;g;b` 也解析（"可选"能力，已实现）。
- CSI 最小子集：`A/B/C/D` 上下左右、`E/F` 换行上下、`G`/`` ` `` 列、`H`/`f` 定位、
  `d` 行、`J`/`K` 清除（0/1/2）、`s`/`u` 保存恢复；私有模式（`?25`、`?1049` 等）忽略。
- OSC（标题、超链接）与 DCS/SOS/PM/APC 整段丢弃，不进入文本；
  未知 CSI、未知 SGR、截断序列只增加 `unsupported`，不影响已渲染内容。
- 输出超过 `MAX_LINES` 行时丢弃最早的行并置 `truncated`；空白尾行不渲染。

渲染：

- 等宽字体 + `small`/`small_line`，`code_block` 背景、`border_l2` 边、`radii.md` 圆角；
- 每个 run 一个 `TextRun`：粗体/斜体/下划线由 SGR 映射，`dim` 用 60% 不透明度，
  `reverse` 交换前景/背景；
- ANSI 16 色**全部由主题 token 推导**（DSH 没有独立的终端 ANSI token，见下表），
  不写死色值；256 色 16–255 用标准 xterm 公式，truecolor 原样使用；
- 容器最大高度 420px、纵向 + 横向滚动；空输出显示"（无终端输出）"。

ANSI → 主题 token 映射（以浅色为例，深色同 token 自动切换）：

| ANSI | 语义 | token |
|---|---|---|
| 0 黑 | 默认前景 | `label_primary` |
| 1 红 | 错误 | `state_error` |
| 2 绿 | 成功 | `state_success` |
| 3 黄 | 警告 | `state_warn` |
| 4 蓝 | 链接/主色 | `link` |
| 5 洋红 | 红 + 蓝混合 | `state_error` + `link` |
| 6 青 | 绿 + 蓝混合 | `state_success` + `link` |
| 7 白/浅灰 | 苍白 | `label_tertiary` |
| 8 亮黑 | 次级文字 | `label_secondary` |
| 9–14 亮色 | 同色相向默认前景混合 25% | 对应 base token + `label_primary` |
| 15 亮白 | 主文字 | `label_primary` |

默认背景用 `code_block`，默认前景 `label_primary`；因此浅色主题下 ANSI 黑（0）用
`label_primary` 保证可见（标准终端里"黑"在浅底上不可读，这是一个有意的取舍）。

## 限制（明确边界）

1. **不是完整终端仿真**：没有折行/自动换行、DECSTBM 滚动区域、备用屏、鼠标/焦点事件、
   光标渲染与选择复制；是"命令输出查看器"而不是可交互终端（可交互终端属于后续 PTY 任务）。
2. **CJK/emoji 按 1 列**：列推进、光标定位与制表位对宽字符不准确；渲染文本本身正确。
   引入 `unicode-width` 会新增依赖，本 crate 冻结依赖，故后移。
3. **SGR 只覆盖常用集**：闪烁、上划线、隐藏、字体切换、冒号子参数（`38:5:196`）
   不支持（计入 `unsupported` 或忽略），不改变正文。
4. **颜色是近似**：DSH 设计 token 没有终端 ANSI 调色板，16 色由语义 token 推导，
   与真实 xterm 配色不会逐色相等；256/truecolor 按行业公式/原值。
5. **Diff 不做语法高亮、折叠未变更行、字符级 diff**；combined diff 不支持（只提示）。
6. **滚动是普通容器**：与聊天列表一样用 GPUI 的 overflow 滚动，没有虚拟化；超大输出靠
   `MAX_LINES` / `MAX_RENDER_LINES` / 最大高度兜底，不是 10k 行级滚动性能方案。
7. **没有交互**：不可选中复制、不可跳转文件、不可展开上下文；这些等 M3 的元素/动作模型。

## 验证方法

当前验收结果（2026-11-11，Windows）：

- `tools\build-env.cmd cargo test -p dshr-ui-gpui`：**43 passed / 0 failed**
  （含现有 markdown/theme 测试，无回归）；
- `tools\build-env.cmd cargo clippy -p dshr-ui-gpui --all-targets -- -D warnings`：通过；
- `tools\build-env.cmd cargo fmt -p dshr-ui-gpui -- --check`：通过。

```powershell
# 1) 单元测试（解析覆盖 + 视图构造 smoke test，无窗口）
tools\build-env.cmd cargo test -p dshr-ui-gpui

# 2) 格式化
tools\build-env.cmd cargo fmt -p dshr-ui-gpui

# 3) clippy（与最终验收同口径）
tools\build-env.cmd cargo clippy -p dshr-ui-gpui --all-targets -- -D warnings
```

测试清单（`cargo test -p dshr-ui-gpui -- --list` 可看到）：

- diff：多文件/多 hunk、两侧行号推进、空 diff、二进制（git 头与裸 `Binary files`）、
  `\ No newline`、CRLF、未知头部降级、颜色 token 断言、超大 diff 只渲染前 2000 行、
  四种形状渲染不 panic。
- terminal：换行/CR 覆盖/BS/TAB、SGR 前景背景与关闭属性、256/truecolor、光标移动与
  `K`/`J` 清除、OSC 丢弃、未知序列降级、MAX_LINES 截断、颜色 token 推导、xterm 公式、渲染 smoke。

### 截图验证（接线后）

组件本身没有独立窗口，截图需要 Lead 在 `chat` 里接上工具卡片后抓窗口：

```powershell
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr
# 另开终端：
$env:TMP='C:\Projects\dshr\.tmp'; $env:TEMP=$env:TMP
powershell -ExecutionPolicy Bypass -File tools\screen.ps1 windows -ProcessId <pid>
powershell -ExecutionPolicy Bypass -File tools\screen.ps1 winshot -Hwnd <hwnd> -Scale 1.75 -Path .tmp\diff-terminal.png
```

`PrintWindow(PW_RENDERFULLCONTENT)` 的方法与注意事项见
[`ui-dsh-alignment.md`](ui-dsh-alignment.md#验证与限制)。

## 给 Lead 的接线建议（chat.rs 不在本任务写域）

工具卡片目前渲染 `conversation-model::card::ToolCard.preview`（最多 6 行文本）。
接原生视图时不需要改卡片折叠逻辑，按 `ToolCard.kind` 分流即可：

1. `CardKind::Diff`：从工具结果的 `value.diff` / `value.patch`（字符串）调用
   `let diff = parse_diff(&text);`，渲染 `diff_view(theme, &diff)`；
   若只有 `added`/`removed` 计数而没有 diff 文本，继续用现有预览行。
2. `CardKind::Terminal`：从 `value.stdout` / `value.stderr` 取原文，调用
   `terminal_view_from_text(theme, &stdout)`；stderr 可拼在 stdout 之后（组件按
   `\r`/SGR 结算，不会破坏已有行）。非 ANSI 的纯日志同样适用。
3. 长输出已经由组件自身上限（`MAX_LINES`=5000 行、最大高度 420px、横向滚动）兜底，
   卡片外层不需要再加截断；如果希望保留 6 行摘要，可继续用 `ToolCard.preview`
   作为折叠态，展开态再挂原生视图。
4. 组件只用 `&Theme`，不持有状态，可直接在 `chat.rs` 的 `render_row` 里调用；
   不做 `cx.notify`、不注册回调，后续要加"展开/复制"时再加动作，避免现在引入状态。

## 未做项 / 后续

- 可交互 PTY 终端（键盘输入、光标、resize、选择复制）；本任务是输出视图。
- Diff 语法高亮、折叠 unchanged、字符级 diff、跳转。
- `unicode-width` 宽字符列推进（需要新增依赖，按 ADR 后移）。
- 与工具卡片折叠态的双向展开交互（属于 chat.rs 的 UI 状态，Lead 集成轮处理）。
