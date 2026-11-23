# M4 最小桌面打包

G1 的发布管线先落地**可复现的二进制归档**，不包含安装器、代码签名、自动更新。

## 1. 命令

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-desktop.ps1 -Configuration debug
bash tools/package-desktop.sh debug
```

`-Configuration release` / `release` 走 release 构建。脚本会：

1. `cargo build -p dshr-desktop-gpui --bin dshr`；
2. 把 `dshr`/`dshr.exe`、`runtime/node`、`sidecar/dsh-official` 与 `README.txt` 放到
   `dist/dshr-<config>-<os>-<arch>/`；
3. 压成 zip（Windows）或 tar.gz（Linux/macOS），并生成同名 `.sha256` 校验文件。

Windows 脚本通过 `tools/build-env.cmd`（TMP/TEMP 处理），Linux 脚本可用
`CARGO=/path/to/cargo` 指定工具链。

Windows 的 debug 和 release 构建都用 Windows SDK 的 `fxc.exe` 预编译并嵌入 GPUI 的 DirectX shader。
构建时可用 `GPUI_FXC_PATH` 指定 `fxc.exe`。解压后的应用可以脱离构建机的 Cargo 缓存启动。
Windows 打包通过 `vswhere.exe` 查找 Visual Studio 的 x64 可再发行 `vcruntime140.dll`，并放在桌面与控制台可执行文件旁。用户解压后即可运行。[运行组件规范](spec/runtime-components.md)

Windows x64 调试包默认从本机 DeepSeek Harness 的 `app.asar` 抽取 Windows 已验收的 `rc.5` 工具和配套依赖。使用 `-DshAsar` 指定安装档，或使用 `-DshNodeModules` 指定由 npm 锁定的依赖目录。两种来源都按同一份来源锁校验。打包时从 `PATH` 或 `-PowerShellExe` 指定的位置复制独立的 x64 PowerShell 7 发行目录，解压后使用包内运行时。首次启动会用包内默认配置生成 `profile/plugins.json`，已有配置继续优先。前台 PowerShell 和 Native 执行器默认启用，持久 PowerShell 与 PTC 可在插件页切换。Office 需要激活 Office Skill，网页调用依赖可用凭据。[兼容矩阵](spec/dsh-compat-mapping.md#兼容矩阵)

从 GitHub Actions 下载时，选择 `desktop-package-windows-latest` 并完整解压其中的 Windows ZIP。`desktop-windows-latest` 提供用于调试的单个可执行文件。把完整调试包解压到新目录并启动即可体验 31 项默认插件，同时保留旧目录的 `profile/plugins.json`。显式设置的 `DSHR_HOME`、`DSHR_PROFILE_DIR` 或 Loader 配置仍按原有优先级读取。

Windows CI 用 npm 锁文件安装官方依赖，再用 Node `26.13.1` 生成只读 `official.zip`。Sidecar 直接加载归档和 `native/node_modules` 中的真实路径。原生依赖随外层发行 ZIP 一起解压。本机可以指定带 ZIP VFS 的 Windows x64 Node，可执行文件通过 `-NodeExe` 传入。省略该参数时使用 `runtime/node/node.exe`。现有 Node 24 仍可用于基础包。

```powershell
npm ci --ignore-scripts --prefix sidecar/rc5-windows
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-desktop.ps1 -Configuration debug -DshNodeModules sidecar/rc5-windows/node_modules -NodeExe (Get-Command node.exe).Source
powershell -NoProfile -ExecutionPolicy Bypass -File tools/verify-package.ps1 -Archive dist/dshr-debug-windows-x64.zip -RequireRc5
```

Windows release、其他平台及显式带 `-BaseSidecar` 的基础调试包，打包前运行 `npm ci --ignore-scripts --prefix sidecar/dsh-official` 安装基础 Sidecar 依赖。Windows x64 release 还需要运行组件目录。以下离线示例把两份 ZIP 和目录文件留在 `.tmp/runtime-component-assets`。

~~~powershell
py tools/build-runtime-components.py --out .tmp/runtime-component-assets
.\tools\package-desktop.ps1 -Configuration release -RuntimeComponentCatalog .tmp/runtime-component-assets/components.json
~~~

在线发行时给生成脚本传入 `--base-url`，指向发行方的 HTTPS 资源目录，并把 `runtime-packages/` 中的两个 ZIP 发布到该 URL 下。离线发行可将 `runtime-packages/` 交给用户放到解压后的应用目录，再从「设置 → 运行组件」按需安装。基础归档只带目录清单。Python 与 Rust 输入版本及 SHA-256 记录在生成脚本中。[运行组件契约](spec/runtime-components.md) · [验收步骤](plan/runtime-components.md)

### 直接启动 Cargo 调试版

准备好上文的 `rc.5` 调试包后，关闭正在运行的 `target/debug/dshr.exe`，然后从仓库根目录执行以下命令。

```powershell
tools\build-env.cmd cargo build --locked -p dshr-desktop-gpui --bin dshr
powershell -NoProfile -ExecutionPolicy Bypass -File tools/stage-windows-debug.ps1
.\target\debug\dshr.exe
```

暂存脚本默认从 `dist/rc5/dshr-debug-windows-x64` 复制已校验的 Sidecar、Node 和 PowerShell。开发版从 `target/debug/profile/plugins.json` 读取插件配置，暂存脚本给这个文件补齐模板缺项。已有配置和开关优先，改写前会备份。若要导入当前使用的其他 profile，可向暂存脚本传入 `-ExistingProfile` 和原文件路径。开发版仍从原数据根载入会话、工作区和用户设置。显式指定的资源根、数据根和 profile 按原有规则优先。

实测直接启动 `target/debug/dshr.exe` 后，插件页显示 31 项，其中 29 项启用。待办开关可以来回切换。开发版文件写入自检和使用暂存 PowerShell 的调用测试均通过。

## 2. 本机实测

切换到只读归档之前，Windows x64 `rc.5` 调试包位于 `dist/rc5/dshr-debug-windows-x64.zip`，大小为 260,941,409 B，SHA-256 为 `79d6b57eed2b379211bf6cae8cad29312321641114fa1d192c221df4de2484b6`。常规启动的插件页显示 31 项，其中 27 项是工具包，4 项是配套插件，共有 29 项默认启用。两组互斥插件均可在插件页来回切换。待办插件停用后从模型工具清单移除，重新启用后成功写入会话待办。桌面聊天也调用了文件读取、文件搜索、Cordis 检查和工作区依赖查询。调用由本地测试模型触发，工具由包内 Sidecar 执行。工作区依赖查询使用本机 DeepSeek Harness 的 primary runtime。

ZIP 校验、窗口与组件自检均通过。包内 PowerShell 7.6.5 在仅包含 Windows 系统目录的 `PATH` 下可启动，官方 PowerShell 工具测试核对了进程返回的 `$PSHOME`。下面的兼容验收直接使用包内 Sidecar，校验了 48 个官方包的版本与散列，27 项测试全部通过。

归档发行的文件数、冷启动和工具兼容验收见 [Sidecar 归档验收](plan/dsh-sidecar-archive.md)。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-desktop.ps1 -Configuration debug -OutDir dist/rc5 -NodeExe (Get-Command node.exe).Source
powershell -NoProfile -ExecutionPolicy Bypass -File tools/verify-package.ps1 -Archive dist/rc5/dshr-debug-windows-x64.zip -RequireRc5
powershell -NoProfile -ExecutionPolicy Bypass -File tools/official-rc5-gate.ps1 -PackagedSidecar dist/rc5/dshr-debug-windows-x64/sidecar/dsh-official
```

历史归档记录如下。

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
- Windows `rc.5` 调试包附带已验收工具的依赖。其他官方工具的兼容范围及限制见[兼容矩阵](spec/dsh-compat-mapping.md#兼容矩阵)。
- 没有安装器/签名/自动更新/差分回滚；G1 仍不能因归档可运行标完成。
