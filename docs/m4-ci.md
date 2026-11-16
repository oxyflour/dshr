# M4 三平台 CI（G1）

G1 要求 Windows/macOS/Linux 管线 + 两类插件来源回归。当前拆成两条 workflow：

| Workflow | Runner | 内容 | 状态 |
|---|---|---|---|
| [`conformance.yml`](../.github/workflows/conformance.yml) | `windows-latest` | schema/links/node-test/cargo 全量、矩阵 artifact、G4 perf gate | 本机等价命令通过；Actions **未实跑** |
| [`cross-platform.yml`](../.github/workflows/cross-platform.yml) | `windows/ubuntu/macos-latest` | 三平台契约 + 核心 crate 测试 + `desktop-build`（二进制 + 自检 + 打包 + `verify-package`） | **新增，Actions 未实跑** |
| [package-desktop.ps1](../tools/package-desktop.ps1) / [.sh](../tools/package-desktop.sh) | 本机 Windows/Linux | 打包 zip/tar + `.sha256`；产物含 runtime/node、probe、dsh-official | 本机 PASS |
| [verify-package.ps1](../tools/verify-package.ps1) / [.sh](../tools/verify-package.sh) | 本机 Windows/Linux | 校验 sha256 + 解压 + sidecar/official 自检 | 本机 PASS |

## 1. cross-platform.yml 覆盖什么

`contract` job（三平台）：

```text
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick --compact
```

`core-rust` job（三平台）：

```text
cargo test --offline \
  -p dshr-agent-context -p dshr-agent-core -p dshr-compat-dsh-host \
  -p dshr-compat-dsh-protocol -p dshr-compat-mcp -p dshr-conversation-model \
  -p dshr-llm-deepseek -p dshr-llm-openai -p dshr-llm-retry \
  -p dshr-plugin-api -p dshr-plugin-host -p dshr-plugin-sdk -p dshr-plugin-wasm \
  -p dshr-profile -p dshr-sandbox -p dshr-session-export -p dshr-session-store \
  -p dshr-tool-runtime -p dshr-ui-contributions
```

其中 `dshr-plugin-host` 覆盖 external-process 退路，`dshr-plugin-wasm` 覆盖 WASM 主机，
即 G1 的"两类插件来源"；`dshr-compat-dsh-host` 的 Node 测试依赖 runner 自带 Node 22。

## 2. 明确不覆盖什么

- `desktop-build` 做 `cargo build`、Linux/macOS 无窗口自检、zip/tar 打包、`.sha256` 与
  `verify-package` 解压自检（含 cpp-v1 component 的 5 项自检）；Windows Authenticode 与
  macOS `codesign` 有**可选**步骤，只有配置 secrets 才执行，当前本机无证书/未实跑。
- Actions 尚未真实执行；本文件只记录 workflow 已落地、YAML 已本地解析（PyYAML），
  不代表 CI 已绿。Linux 的 `desktop-build` 等价命令已在 WSL 本机通过 `cargo build`
  （见 [m4-linux.md](m4-linux.md)），macOS/Windows runner 未验证。
- 签名 secrets：`WINDOWS_CERT_PFX_BASE64`/`WINDOWS_CERT_PASSWORD`（Windows），
  `MACOS_SIGN_IDENTITY`（macOS，codesign 身份）；没有 secrets 时签名步骤跳过，
  不会把 unsigned 误当 signed。macOS 公证（notarization）仍未实现。

## 3. 本地等价命令

```text
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick --compact
```

核心 crate 的 Windows 等价命令见上面 `cargo test`；macOS/Linux 需要在对应 runner 上跑，
本机 Windows 不能替代。

## 4. 官方包来源/摘要

`tools/official-provenance.mjs` 用
[`official-packages.lock.json`](../sidecar/dsh-official/official-packages.lock.json) 校验
`dsh-tool-working-directory` / `dsh-tool-todo` / `dsh-storage-domain` 的版本与
`package.json`、`lib/index.js` SHA-256；`official-l2-gate.ps1` 在跑测试前强制执行，
不匹配直接 exit 1。升级官方包时必须：

```text
node tools/official-provenance.mjs --update   # 重写 lock
node tools/official-provenance.mjs --check    # 复查
```

lock 与 fixture 版本绑定，因此仍不能替代"官方发行渠道校验"（CA/签名/registry），
只是把 N4 的"形状正确即可"堵住。


## 5. 何时可以把 G1 标完成

必须同时满足：

1. `conformance.yml` 在 GitHub Actions（Windows）实跑绿；
2. `cross-platform.yml` 的 `contract` + `core-rust` 在三个 OS 实跑绿；
3. 桌面 GPUI 的 macOS/Linux 构建/打包管线有独立 job（当前没有）；
4. 两类插件来源在三个 OS 都有回归证据（当前只有 Windows 本机 + 计划中的 core matrix）。

在此之前 G1 保持“未验证”。
