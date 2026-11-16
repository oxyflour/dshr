# M4 macOS 交叉检查（Linux 上，无 macOS SDK）

本机没有 macOS runner/SDK；用 Rust 标准库 target 做**类型检查**，不等价于 macOS 构建/运行。

## 1. 命令

```text
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo check --target x86_64-apple-darwin -p dshr-ui-gpui
cargo check --target aarch64-apple-darwin -p dshr-ui-gpui
```

结果：**两个 target 都 PASS**（各约 34s，链接不执行），`dshr-ui-gpui` 在 Intel/Apple Silicon
macOS target 上类型检查通过。这覆盖 GPUI/ADC 渲染器的 `cfg(target_os = "macos")` 分支。

## 2. 桌面二进制

```text
cargo check --target x86_64-apple-darwin -p dshr-desktop-gpui --bin chat
```

结果：**FAIL**，但失败在 C 依赖的交叉编译，不是 Rust 代码：

- `zstd-sys` 的 `cc` 不认识 `-arch x86_64` / `-mmacosx-version-min`；
- `ring` 的 `pregen` 汇编同样需要 macOS clang/SDK；
- 本机只有 Linux `cc`，没有 macOS SDK。

因此 **不能**把这条当成 macOS 桌面通过；它只证明纯 Rust 的 UI 部分可类型检查。
仍需要在 macOS runner 上跑 `cross-platform.yml` 的 `desktop-build`。

## 3. 边界

- 无链接、无运行、无签名/公证、无 `.app` 打包。
- G1 的 macOS 行仍为未验证；本文件只记录交叉类型检查证据。
