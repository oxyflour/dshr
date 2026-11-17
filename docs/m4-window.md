# M4 真实窗口 smoke test

`chat --selftest-window` 真正创建 GPUI 窗口、等待 800ms 首帧，然后自动退出。

## 实测

```text
.\target\debug\chat.exe --selftest-window
[selftest-window] PASS
exit 0
```

- 本机 Windows 桌面会话：**PASS**。
- GitHub Actions Windows runner（run 44138465346 的 `desktop build (windows-latest)`）：也 **PASS**。
- 只覆盖窗口创建/事件循环/首帧，不覆盖输入法、剪贴板、键盘全流程或 GPU 合成。
- G3 仍需真实桌面人工按键验收；窗口 smoke 只是必要非充分证据。
