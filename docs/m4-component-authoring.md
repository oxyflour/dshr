# cpp-v1 WASM Component 作者指南（M4 W5）

## 1. 生成项目

```text
node tools/new-component-plugin.mjs --out dist/component-demo \
  --id demo.echo --name "Demo Echo" --version 0.1.0
```

模板位于 [templates/wasm-component-plugin](../templates/wasm-component-plugin)：
`Cargo.toml`、`src/lib.rs`、`plugin.toml`、`wit/cpp-v1.wit`、`README.md`。
生成器替换 crate/id/name/version 占位符，并保留 WIT 副本（项目自包含）。

## 2. 构建

```text
rustup target add wasm32-wasip2
cd dist/component-demo
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/demo_echo.wasm plugin.wasm
```

本机实测：生成 `demo.echo` 项目，release wasm **74 KB**，构建 exit 0。

## 3. 安装边界

```text
dshr chat --install-plugin <dir>
```

宿主会读 `plugin.toml` 的 `runtime.kind=wasm-component` 与 `entry`，交给
`CppComponentPlugin`；工具由 guest 的 `list-tools` 声明（模板提供 `echo`）。
当前尚未在真实 chat 窗口里跑安装/调用的手工验收，也没有组件签名/公证；
本指南只覆盖“生成 → 构建 → 可被 loader 读取”的作者路径。
