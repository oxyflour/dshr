# M4 真实窗口 smoke test

`chat --selftest-window` 真正创建 GPUI 窗口、等待 800ms 首帧，然后自动退出。

## 实测

```text
.\target\debug\chat.exe --selftest-window
[selftest-window] PASS
exit 0
```

- 本机 Windows 桌面会话：**PASS**。
- 只覆盖窗口创建/事件循环/首帧，不覆盖输入法、剪贴板、键盘全流程或 GPU 合成。
- 没有加入 CI：GitHub Windows runner 是无交互会话，窗口创建结果不可靠；
  G3 仍需真实桌面人工按键验收。
