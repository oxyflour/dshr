# M4 真实剪贴板 smoke test

`chat --selftest-clipboard` 通过 GPUI/OS 剪贴板写入并读回 UTF-8 文本（含中文）：

```text
.\target\debug\chat.exe --selftest-clipboard
[selftest-clipboard] PASS text="dshr clipboard selftest 中文"
exit 0
```

- 本机 Windows 桌面会话 **PASS**。
- GitHub Actions Windows runner（run 44138465346 的 `desktop build (windows-latest)`）也 **PASS**。
- 覆盖真实系统剪贴板读写与 Unicode；不覆盖富文本/图片、剪贴板权限拒绝、Linux Wayland。
