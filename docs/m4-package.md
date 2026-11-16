# M4 最小桌面打包

G1 的发布管线先落地**可复现的二进制归档**，不包含安装器、代码签名、自动更新。

## 1. 命令

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-desktop.ps1 -Configuration debug
bash tools/package-desktop.sh debug
```

`-Configuration release` / `release` 走 release 构建。脚本会：

1. `cargo build -p dshr-desktop-gpui --bin chat`；
2. 把 `chat`/`chat.exe`、`README.txt`（配置、commit、构建时间、自检命令）放到
   `dist/dshr-<config>-<os>-<arch>/`；
3. 压成 zip（Windows）或 tar.gz（Linux/macOS）。

Windows 脚本通过 `tools/build-env.cmd`（TMP/TEMP 处理），Linux 脚本可用
`CARGO=/path/to/cargo` 指定工具链。

## 2. 本机实测

| 平台 | 产物 | 大小 |
|---|---|---|
| Windows x64（debug） | `dist/dshr-debug-windows-x64.zip` | 56,612,384 B |
| Linux x86_64（debug） | `dist/dshr-debug-linux-x86_64.tar.gz` | 195,024,785 B |

归档内含 `runtime/node`（vendored Node）与 `sidecar/probe-*.mjs`；Windows 二进制来自本机
`cargo build`，Linux 二进制来自 WSL（见 [m4-linux.md](m4-linux.md)），两者都 exit 0。

## 3. 包内 smoke test

Windows 归档解压后，**不再设置 `DSHR_NODE`**，直接运行：

```powershell
cd dist/dshr-debug-windows-x64
.\chat.exe --selftest-sidecar-recovery
```

实测 `live=true killed=true dead_call=true native=true replayed=3`，PASS；日志显示
`sidecar node=...\runtime\node\node.exe（Vendored）`。

Linux 归档解压后同样直接运行：

```bash
cd dist/dshr-debug-linux-x86_64
./chat --selftest-sidecar-recovery
```

实测同样 PASS，`sidecar node=.../runtime/node/node（Vendored）`。即两个包对 probe
sidecar 已自带运行时，不再依赖外部 Node。

## 4. 明确未做

- 没有 macOS `.app`/签名/公证；Windows 没有 MSI/NSIS 或 Authenticode；Linux 没有 deb/AppImage。
- 归档含 `chat`、`runtime/node` 与 probe sidecar，但不含 `dsh-official/node_modules`；
  完整 DSH 官方工具仍需把官方 sidecar 依赖一起打包。
- 没有自动更新/差分/回滚。
- CI 尚未实跑打包脚本；本机 Windows/Linux 已验证。

G1 仍不能因为这两个归档标完成。
