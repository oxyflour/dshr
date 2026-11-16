# __NAME__

`cpp-v1.wit` WASM Component 插件模板（M4 W5）。

## 构建

```text
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/__CRATE_NAME__.wasm plugin.wasm
```

## 安装

```text
dshr chat --install-plugin <this-dir>
```

安装时宿主读取 `manifest.json`，把 `plugin.wasm` 交给 `CppComponentPlugin`；
工具由 guest 的 `list-tools` 声明（当前模板提供 `echo`），不需要在 manifest 里重复。
