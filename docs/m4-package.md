# M4 最小桌面打包

G1 的发布管线先落地**可复现的二进制归档**，不包含安装器、代码签名、自动更新。

## 1. 命令

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-desktop.ps1 -Configuration debug
bash tools/package-desktop.sh debug
```

`-Configuration release` / `release` 走 release 构建。脚本会：

1. `cargo build -p dshr-desktop-gpui --bin chat`；
2. 把 `chat`/`chat.exe`、`runtime/node`、`sidecar/probe-*`、`sidecar/dsh-official`（含
   node_modules）与 `README.txt` 放到 `dist/dshr-<config>-<os>-<arch>/`；
3. 压成 zip（Windows）或 tar.gz（Linux/macOS），并生成同名 `.sha256` 校验文件。

Windows 脚本通过 `tools/build-env.cmd`（TMP/TEMP 处理），Linux 脚本可用
`CARGO=/path/to/cargo` 指定工具链。

## 2. 本机实测

| 平台 | 产物 | 大小 | SHA-256 |
|---|---|---|---|
| Windows x64（debug） | `dist/dshr-debug-windows-x64.zip` | 62,721,650 B | `fda23b34…efba` |
| Linux x86_64（debug） | `dist/dshr-debug-linux-x86_64.tar.gz` | 213,631,438 B | `aab3d873…13a4` |

归档内含 `runtime/node`、`sidecar/probe-*.mjs`、完整 `sidecar/dsh-official`（含
node_modules）与 `components/cpp-component`（`plugin.toml` + `package.json` + 真实
wasm32-wasip2 `plugin.wasm`，77,217 B）；Windows 二进制来自本机 `cargo build`，
Linux 二进制来自 WSL（见 [m4-linux.md](m4-linux.md)），两者都 exit 0。校验文件与归档同目录。

## 3. 包内 smoke test

Windows 归档解压后，**不再设置 `DSHR_NODE`**，直接运行：

```powershell
cd dist/dshr-debug-windows-x64
.\chat.exe --selftest-sidecar-recovery
```

实测 `live=true killed=true dead_call=true native=true replayed=3`，PASS；日志显示
`sidecar node=...\runtime\node\node.exe（Vendored）`。再跑官方 sidecar 自检：

```powershell
.\chat.exe --selftest-official
```

实测 `[selftest-official] PASS protocol=2 active=true host_services=3`。再跑组件自检：

```powershell
.\chat.exe --selftest-cpp-component
.\chat.exe --selftest-cpp-install
```

实测两条都 PASS：`registry=echo`，并返回 guest 的 args；`--selftest-cpp-install` 走
`install_package` → profile → loader。

Linux 归档解压后同样直接运行：

```bash
cd dist/dshr-debug-linux-x86_64
./chat --selftest-sidecar-recovery
./chat --selftest-official
./chat --selftest-cpp-component
./chat --selftest-cpp-install
```

四项均 PASS，日志显示 `runtime/node/node（Vendored）` 与 `protocol=2 active=true
host_services=3`；组件自检注册 `echo` 并返回 args。即两个包对 probe、官方 sidecar
与 cpp-v1 component 都已自带运行时，不依赖外部 Node。

## 4. 包校验脚本

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/verify-package.ps1 -Archive dist/dshr-debug-windows-x64.zip
bash tools/verify-package.sh dist/dshr-debug-linux-x86_64.tar.gz
```

脚本会校验 `.sha256`、解压到临时目录、跑 `--selftest-sidecar-recovery`、
`--selftest-official`、`--selftest-cpp-component` 与 `--selftest-cpp-install`，最后清理。
Windows/Linux 本机均 **PASS**。

## 5. 明确未做

- 没有 macOS `.app`/签名/公证；Windows 没有 MSI/NSIS 或 Authenticode；Linux 没有 deb/AppImage。
- 归档含 `chat`、`runtime/node`、probe sidecar、`dsh-official`（含 node_modules）与
  `components/cpp-component`；但 `.tmp/dsh-tools-all` 等本机工具包不随包，官方工具全量
  仍依赖这些 fixture。
- 没有安装器/签名/自动更新/差分回滚；G1 仍不能因归档可运行标完成。
