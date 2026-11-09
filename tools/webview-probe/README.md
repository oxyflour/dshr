# webview-probe — M1 A3 WebView2 探针

**来源**：从 `gpui-kit` `examples/webview/src/main.rs` @ `dbb0b9a` 复制，改动：

1. 依赖改为 git pin（`gpui-kit` / `gpui-webview` / `wry` = `lb-wry`）；
2. 增加页面加载事件打印（`[webview] started/finished <url> t=<ms>`）；
3. 在 WebView 上方**故意压一条 GPUI 横幅**，用来观察浮层是否被页面盖住；
4. 初始 URL 改为 `https://example.com`（白页，便于观察）。

**构建与运行**

```powershell
tools\build-env.cmd cargo build --manifest-path tools\webview-probe\Cargo.toml
.\tools\webview-probe\target\debug\webview-probe.exe
```

冷构建约 6.5 分钟（含 wry / WebView2）。

## 自动可验证的部分（已通过）

| 项 | 结果 |
|---|---|
| WebView2 运行时 | 已安装，`151.0.4129.107` |
| 构建 | 通过（含 `GPUI_DISABLE_DIRECT_COMPOSITION=true`，上游要求） |
| 启动 | 通过（窗口存活 10 s 无崩溃） |
| **引擎自检（IPC）** | 页面 JS 回传 `ua=Edge/151`、`dpr=1.75`、视口 `982×638` |

自检页面在 `with_html` 里注入，脚本立即执行并通过 `window.ipc.postMessage` 回报，
因此不依赖人眼即可确认 WebView2 引擎、JS 与 IPC 通道都正常。

## 需要人工观察的部分（未验证）

逐项对照 GPUI Kit 文档（`website/docs/webview.md`）的已知限制：

| # | 观察点 | 预期（按上游文档） |
|---|---|---|
| 1 | 页面本身是否可见 | 可见；不可见则说明 DirectComposition 没关掉 |
| 2 | 红色边框的 `GPUI OVERLAY` 横幅是否可见 | **不可见**：原生子窗口在 GPUI 内容之上，同矩形内遮住浮层 |
| 3 | 窗口缩放后页面是否跟随 | 应跟随；若错位则记录具体尺寸 |
| 4 | 点击页面后再按 Tab 回到 GPUI 输入框 | 焦点应可回到 GPUI；记录是否卡死在页面 |
| 5 | 把窗口缩到 WebView 小于容器 | Windows 上原生视图**不会被裁剪**，会溢出容器 |
| 6 | 125%/150% 缩放显示下页面的清晰度 | 记录模糊/尺寸偏差 |

把结果填回 [`../../docs/m1-log.md`](../../docs/m1-log.md) 的 A3 小节。
