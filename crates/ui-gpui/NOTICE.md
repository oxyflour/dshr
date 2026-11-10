# 第三方代码声明

## GPUI 文本输入组件（`src/text_input.rs`）

- 来源：`zed-industries/zed` 的 `crates/gpui/examples/input.rs`
- 固定 commit：`d2a4fb6ded7c470be25d1541281da3882495b65b`（与 `ADR-0001` 的 GPUI pin 同一版本）
- 许可：Apache-2.0（上游示例以 Apache-2.0 分发）
- 本仓库改动：
  1. 增加 `Submit` action 与 `on_submit` 回调（回车发送、失败保留内容）；
  2. 去掉示例窗口、字体加载、退出键与演示区块；尺寸/配色交由外层容器；
  3. 占位符颜色改为深色界面可读。
- 保留的原始实现：UTF-16 选区换算、marked text（IME 组合态）、光标/选区绘制、
  剪贴板动作、`ElementInputHandler` 注册。

GPUI 本身通过 git 依赖引入并在 `docs/decisions/0001-gpui-pin.md` 固定版本。
