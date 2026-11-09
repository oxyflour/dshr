# ime-probe — M1 A2 中文 IME 探针

**来源**：从 zed `crates/gpui/examples/input.rs` @ `d2a4fb6` 原样 vendor（Apache-2.0）。
改动只有两处：删掉 wasm 专用的 `example_support` 字体模块与 `load_fonts` 调用，其余保持上游行为。

**为什么复制而不是自己写**：GPUI 这个版本没有内置 `TextInput`，IME 必须由插件/应用实现
`EntityInputHandler` 并自行处理 marked text、UTF-16 选区、候选窗定位。上游示例已是这套逻辑的
完整参考实现，重写它只会引入误差。

**构建与运行**

```powershell
tools\build-env.cmd cargo build --manifest-path tools\ime-probe\Cargo.toml
.\tools\ime-probe\target\debug\ime-probe.exe
```

**这个探针要回答什么**

1. 中文输入法（微软拼音）的组合态能否正常显示下划线、光标是否跟随；
2. 候选窗是否出现在正确位置（GPUI 自绘输入框没有原生 IME 候选窗定位，这是最大风险点）；
3. 上屏后文本是否正确、删除/选词是否破坏 UTF-16 选区；
4. 切换中英文、以及输入法关闭时是否退化为普通键盘输入。

**当前状态**：编译通过、可启动（窗口保持存活）。**IME 行为尚未人工验证** —— 需要真实键盘输入，
无法用脚本代替。验证结果应写回 [`../../docs/m1-log.md`](../../docs/m1-log.md) 的 A2 小节。
