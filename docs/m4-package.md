# M4 最小桌面打包

G1 的发布管线先落地**可复现的二进制归档**，不包含安装器、代码签名、自动更新。

## 1. 命令

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-desktop.ps1 -Configuration debug
bash tools/package-desktop.sh debug
```

`-Configuration release` / `release` 走 release 构建。脚本会：

1. `cargo build -p dshr-desktop-gpui --bin dshr`；
2. 把 `dshr`/`dshr.exe`、`runtime/node`、`sidecar/dsh-official`（含
   node_modules）与 `README.txt` 放到 `dist/dshr-<config>-<os>-<arch>/`；
3. 压成 zip（Windows）或 tar.gz（Linux/macOS），并生成同名 `.sha256` 校验文件。

Windows 脚本通过 `tools/build-env.cmd`（TMP/TEMP 处理），Linux 脚本可用
`CARGO=/path/to/cargo` 指定工具链。

Windows 的 debug 和 release 构建都用 Windows SDK 的 `fxc.exe` 预编译并嵌入 GPUI 的 DirectX shader。
构建时可用 `GPUI_FXC_PATH` 指定 `fxc.exe`。解压后的应用可以脱离构建机的 Cargo 缓存启动。
Windows 打包通过 `vswhere.exe` 查找 Visual Studio 的 x64 可再发行 `vcruntime140.dll`，并放在桌面与控制台可执行文件旁。用户解压后即可运行。[运行组件规范](spec/runtime-components.md)

打包前运行 `npm ci --ignore-scripts --prefix sidecar/dsh-official` 安装锁定的官方 Sidecar 依赖。Windows 可运行 `node tools/vendor-node.mjs` 准备 Node。Windows x64 release 还需要运行组件目录。以下离线示例把两份 ZIP 和目录文件留在 `.tmp/runtime-component-assets`。

~~~powershell
py tools/build-runtime-components.py --out .tmp/runtime-component-assets
.\tools\package-desktop.ps1 -Configuration release -RuntimeComponentCatalog .tmp/runtime-component-assets/components.json
~~~

在线发行时给生成脚本传入 `--base-url`，指向发行方的 HTTPS 资源目录，并把 `runtime-packages/` 中的两个 ZIP 发布到该 URL 下。离线发行可将 `runtime-packages/` 交给用户放到解压后的应用目录，再从「设置 → 运行组件」按需安装。基础归档只带目录清单。Python 与 Rust 输入版本及 SHA-256 记录在生成脚本中。[运行组件契约](spec/runtime-components.md) · [验收步骤](plan/runtime-components.md)

## 2. 本机实测

| 平台 | 产物 | 大小 | SHA-256 |
|---|---|---|---|
| Windows x64（debug） | `dist/dshr-debug-windows-x64.zip` | 65,240,414 B | `39a53cf2…c1a` |
| Linux x86_64（debug） | `dist/dshr-debug-linux-x86_64.tar.gz` | 213,630,798 B | `f897422d…10a2` |

上述是旧版归档记录，当时提供 `sidecar/probe-*.mjs`。归档也包含 `runtime/node`、完整 `sidecar/dsh-official`（含
node_modules）与 `components/cpp-component`（`plugin.toml` + `package.json` + 真实
wasm32-wasip2 `plugin.wasm`，77,217 B）；Windows 二进制来自本机 `cargo build`，
Linux 二进制来自 WSL（见 [m4-linux.md](m4-linux.md)），两者都 exit 0。校验文件与归档同目录。

## 3. 包内 smoke test

Windows 归档解压后，直接使用包内 Node 运行官方 sidecar 自检。

```powershell
cd dist/dshr-debug-windows-x64
.\dshr.exe --selftest-official
```

实测 `[selftest-official] PASS protocol=2 active=true host_services=3`。再跑组件自检：

```powershell
.\dshr.exe --selftest-cpp-component
.\dshr.exe --selftest-cpp-install
.\dshr.exe --selftest-cpp-cli-install
```

实测三条都 PASS：`registry=echo`，并返回 guest 的 args；`--selftest-cpp-install` 走
`install_package` → profile → loader；`--selftest-cpp-cli-install` 走信任安装 CLI
（`signature=missing`）→ profile → loader。

Linux 归档解压后同样直接运行：

```bash
cd dist/dshr-debug-linux-x86_64
./dshr --selftest-official
./dshr --selftest-cpp-component
./dshr --selftest-cpp-install
./dshr --selftest-cpp-cli-install
```

历史 Linux 冒烟日志记录了 `runtime/node/node（Vendored）` 与 `protocol=2 active=true host_services=3`。
组件自检注册 `echo` 并返回参数，CLI 安装自检报告 `signature=missing`。

## 4. 包校验脚本

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/verify-package.ps1 -Archive dist/dshr-debug-windows-x64.zip
bash tools/verify-package.sh dist/dshr-debug-linux-x86_64.tar.gz
```

两个平台的脚本都会校验 `.sha256`、解压到临时目录并检查探针脚本缺席，再执行
`--selftest-official`、`--selftest-cpp-component`、`--selftest-cpp-install` 与
`--selftest-cpp-cli-install`，最后清理。Windows 还会用 `--selftest-window` 验证 DirectX 初始化和窗口首帧。
探针故障恢复用例 `--selftest-sidecar-recovery` 从仓库运行，CI 在打包前执行。

## 5. 明确未做

- 没有 macOS `.app`/签名/公证；Windows 没有 MSI/NSIS 或 Authenticode；Linux 没有 deb/AppImage。
- 归档含 `dshr`、`runtime/node`、`dsh-official`（含 node_modules）与
  `components/cpp-component`；但 `.tmp/dsh-tools-all` 等本机工具包不随包，官方工具全量
  仍依赖这些 fixture。
- 没有安装器/签名/自动更新/差分回滚；G1 仍不能因归档可运行标完成。
