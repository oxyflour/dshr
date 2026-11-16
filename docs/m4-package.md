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
| Windows x64（debug） | `dist/dshr-debug-windows-x64.zip` | 21,657,077 B |
| Linux x86_64（debug） | `dist/dshr-debug-linux-x86_64.tar.gz` | 152,695,054 B |

两者都 exit 0。Linux 二进制来自 WSL 的 `cargo build`（见 [m4-linux.md](m4-linux.md)），
并已通过 ADC/恢复/崩溃自检。

## 3. 包内 smoke test

Windows 归档解压后，把 Node 显式指到仓库 vendored 运行时（归档本身不含 Node）：

```powershell
$env:DSHR_NODE = "C:\Projects\dshr\runtime\node\node.exe"
cd dist/dshr-debug-windows-x64
.\chat.exe --selftest-sidecar-recovery
```

实测 `live=true killed=true dead_call=true native=true replayed=3`，PASS。说明包的
`chat.exe + sidecar/probe-*` 布局可运行；缺的是 Node runtime 与 dsh-official node_modules。

## 4. 明确未做

- 没有 macOS `.app`/签名/公证；Windows 没有 MSI/NSIS 或 Authenticode；Linux 没有 deb/AppImage。
- 归档只含 `chat` 二进制与 probe sidecar，不含 Node/sidecar runtime；需 `DSHR_NODE` 或 PATH
  提供 Node，完整可分发包仍需把 runtime 资产一起打包。
- 没有自动更新/差分/回滚。
- CI 尚未实跑打包脚本；本机 Windows/Linux 已验证。

G1 仍不能因为这两个归档标完成。
