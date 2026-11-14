# __PLUGIN_NAME__

CPP-v1 `external-process` 插件模板：一个 `__TOOL_NAME__` 工具 + 一个 `workspace.panel` 面板。
工具名由插件名派生（`[a-z][a-z0-9_]*`，全局唯一），避免多个插件都用 `echo` 时冲突。
不需要读主程序源码，按本 README 与 `docs/spec/` 即可构建、打包、安装。

- 目标宿主：dshr（Windows，CPP-v1 / `cpp-process/v1`）
- 插件 ID：`__PLUGIN_ID__`（= 包名；与 `plugin.toml` 的 `[plugin].id` 一致）
- 可执行文件：`bin/__BIN_NAME__.exe`（`[runtime].entry` 指向它）

## 目录结构

```text
.
├── Cargo.toml        # 独立 workspace；SDK 走 path 依赖（生成器已注入仓库路径）
├── src/main.rs       # __TOOL_NAME__ 工具 + workspace.panel 的 ui.render 元素树
├── plugin.toml       # CPP-v1 manifest：id/version/runtime/权限
├── package.json      # name/version（必须与 plugin.toml 一致）
├── bin/              # 构建后把 __BIN_NAME__.exe 复制到这里
└── README.md
```

## 构建

```powershell
# 建议把构建产物放到插件目录外，避免 target/ 被计入包摘要
$env:CARGO_TARGET_DIR = "<你的缓存目录>\__CRATE_NAME__"
cargo build --release
Copy-Item "$env:CARGO_TARGET_DIR\release\__BIN_NAME__.exe" "bin\__BIN_NAME__.exe"
```

## 打包与安装

```powershell
# 1) 打包：校验 package.json/plugin.toml，计算 dshr-trust/v1 摘要并写 dshr-trust.json
node <dshr>\tools\plugin-pack.mjs --plugin . --out ..\pkg-__CRATE_NAME__

# 2) 安装：未签名包首次安装需要 --yes-install（会展示预览并写审计）
<dshr>\target\debug\chat.exe --install-plugin ..\pkg-__CRATE_NAME__ --yes-install

# 3) 查看审计：<profile>/install-audit.jsonl
```

本地开发可跳过信任链：直接把**包目录**（含 `dshr-trust.json`）交给宿主/测试加载；
正式安装仍建议走 `chat --install-plugin`。

## 改什么

- 改工具：`src/main.rs` 里的 `ToolDecl::new(...)` 与 handler 的 `match call.tool`。
- 改面板：`PanelDecl::new(...)` 与 `panel_tree()`；元素字段见
  `docs/spec/adc-v1.md` §6、`schema/adc-v1/element.schema.json`。
- 改名字/版本：同时改 `plugin.toml` 与 `package.json`（必须一致），再改工具名/面板 id。
- 配额：`plugin.toml` 的 `[runtime] max_memory_mb/timeout_ms`（spec §12.4）。

## 协议速查（`cpp-process/v1`）

```text
host → process.hello   → child
child → process.ready  { tools[], panels[] }
host → tool.invoke     { tool, arguments, timeout_ms, cancelable }
child → tool.result    { content[], value?, meta?, is_error }
host → process.ping ↔ process.pong
host → process.shutdown → child → process.bye
```

日志一律写 stderr（stdout 是协议通道）。协议全文见 `docs/spec/cpp-v1.md` §12。

## 文档

- 作者入口：`docs/plugin-authoring.md`
- 协议：`docs/spec/cpp-v1.md` §12
- 元素词汇表：`docs/spec/adc-v1.md` §6
- 信任与安装：`docs/spec/plugin-trust.md`
