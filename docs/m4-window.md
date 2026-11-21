# M4 真实窗口 smoke test

`dshr --selftest-window` 真正创建 GPUI 窗口、等 500ms 后通过 `window.defer` 注入
一个 `Enter` 键，再等 500ms 校验 key handler 被调用，然后自动退出。

## 实测

```text
.\target\debug\dshr.exe --selftest-window
[selftest-window] PASS window+keyboard
exit 0
```

- 本机 Windows 桌面会话：**PASS**。
- GitHub Actions Windows runner（run 44138465346 的 `desktop build (windows-latest)`）：也 **PASS**。
- 覆盖窗口创建/事件循环/首帧/真实 `dispatch_keystroke` 路由；不覆盖 ChatApp 主流程、
  输入法、剪贴板语义或 GPU 合成。
- 直接在一个 `WindowHandle::update` 里 dispatch 会触发 GPUI 实体重入 panic；
  必须 `window.defer` 到本帧更新之后再注入（见 [m4-keyboard.md](m4-keyboard.md) §5）。
- G3 仍需真实桌面人工按键验收；窗口/键盘 smoke 只是必要非充分证据。
