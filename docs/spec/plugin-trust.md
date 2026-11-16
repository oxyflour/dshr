# 插件信任链（最小版）— 来源校验、完整性、首装确认与降级拒绝

> 状态：Review（M3 最小版；M4 W1 扩展密钥生命周期） · 版本：`dshr-trust/v1` · 关联：[`cpp-v1.md`](cpp-v1.md)、[`adc-v1.md`](adc-v1.md)、
> [`../decisions/0009-plugin-trust.md`](../decisions/0009-plugin-trust.md)、[`../decisions/0013-m4-scope.md`](../decisions/0013-m4-scope.md)、[`../../schema/cpp-v1/trust.schema.json`](../../schema/cpp-v1/trust.schema.json)、[`../../schema/cpp-v1/trust-keys.schema.json`](../../schema/cpp-v1/trust-keys.schema.json)
> 适用对象：所有经 dshr 安装的非内建插件包（CPP-v1 原生包、WASM 包、DSH JS 包）。

---

## 1. 范围与威胁模型

M3 的"最小信任链"只回答三个问题，不做完整证书体系：

1. **包是否被篡改**（完整性）：安装前对包内容计算摘要，与发布方声明的摘要比对。
2. **包是否来自声明者**（来源，可选）：发布方可对摘要签名；宿主用本地信任密钥库验证。
   M4 W1 起密钥支持**时间窗、吊销与轮换**（`key_id` 不变时也可换钥重签）。
3. **用户是否知情**（首装确认/审计）：首次安装或未验证签名的包必须逐项确认并留审计；
   卸载也写审计（`action: "uninstall"`）。

不承诺（明确出 M4 基线或 M5）：CA/证书链、时间戳/透明日志、包市场评分、
网络来源验证（安装仍以本地目录/离线 `.tgz` 为主）。

**威胁模型**：攻击者能改写安装介质（下载目录、U 盘、构建产物被替换），但不能改写宿主信任密钥库。
因此摘要强制、签名可选、首装确认是"防误装与防篡改"，不防"用户主动安装恶意包"。

## 2. 包级信任文件

签名不能放在 `plugin.toml` / `package.json` 里——那会自引用（文件内容包含自身摘要）。
`dshr-trust/v1` 用**同目录的兄弟文件** `dshr-trust.json`（UTF-8，无 BOM）：

```json
{
  "schema_version": 1,
  "plugin_id": "cad.validate",
  "version": "0.1.0",
  "package": { "digest_sha256": "9f86d0…（64 位小写 hex）" },
  "signature": {
    "scheme": "ed25519",
    "key_id": "cad-vendor-2026",
    "value_hex": "…（128 位小写 hex）"
  }
}
```

规则：

- `plugin_id` / `version` 必须与包的实际清单（`plugin.toml` 或 `package.json`）一致，不一致即
  `E_TRUST_MANIFEST`（防"套壳替换"）。
- `package.digest_sha256` 是**包摘要**，算法见 §3；`dshr-trust.json` 自身**不参与**摘要。
- `signature` 可选；存在时必须同时有 `package.digest_sha256`（否则 `E_TRUST_MANIFEST`）。
- 未知字段拒绝；`schema_version` 只接受 `1`（v1 不向前兼容未知版本）。

## 3. 包摘要（`dshr-trust/v1` 规范化）

对包根目录做确定性遍历，SHA-256：

1. 只收**普通文件**；符号链接/硬链接/设备文件一律不参与（安装时也被拒绝/跳过，见 `archive.rs`）。
2. 跳过固定路径（**仅包根**）：`<root>/dshr-trust.json`（签名文件自身）、`<root>/.dshr-install.json`
   （本机安装元数据，不属于包内容）；`.git/`、`node_modules/`（依赖另行安装，不随包分发）、
   任何以 `.` 开头的目录（编辑器/暂存残留不作为包内容承诺）。
   **嵌套同名文件参与摘要**（例如 `vendor/dshr-trust.json` 不是签名文件，是普通内容）；
   Rust `package_digest` 与 `tools/plugin-pack.mjs` 必须逐字节一致（有交叉测试）。
3. 相对路径统一用 `/` 分隔、按**字节序升序**排序；路径必须是包内相对路径（不解析 `..`）。
4. 对每个文件依次 update：
   `path_utf8` + `0x00` + `size_decimal_utf8` + `0x00` + `file_bytes`。
5. 结果取小写 hex（64 字符）。空包（无文件）摘要为 SHA-256 空输入的 hex。

摘要只承诺**内容**：mtime/权限/所有者不参与（跨平台可复现）。

## 4. 签名（可选，ed25519）

- 签名输入（UTF-8）：
  ```text
  dshr-trust/v1\n<plugin_id>\n<version>\n<digest_sha256_hex>
  ```
  其中 `\n` 是换行符（0x0A）。签名/公钥均为 32/64 字节，`value_hex` 为小写 hex。
- 验证密钥来自**本地信任密钥库**（不进包）：
  - 默认路径 `<profile_dir>/trust-keys.json`：
    ```json
    {
      "schema_version": 1,
      "keys": [
        {
          "key_id": "cad-vendor-2026",
          "public_key_hex": "…（64 hex）",
          "note": "可选",
          "not_before_ms": 1763000000000,
          "not_after_ms": 1794536000000,
          "revoked": false,
          "revoked_at_ms": null
        }
      ]
    }
    ```
  - `key_id` 重复即加载失败；公钥必须 64 位小写 hex；`not_before_ms > not_after_ms`
    即加载失败。时间窗/吊销字段缺省时等价于 M3 的"永久有效、未吊销"（旧文件可读）。
  - 密钥库写入是**原子替换**（临时文件 + 备份 rename）；变更写
    `<profile_dir>/trust-key-audit.jsonl`（`action: add|remove|revoke|rotate`）。
- 验证顺序：摘要匹配 → 密钥存在 → **吊销/时间窗** → 签名有效。任一失败即拒绝安装：

| 结果 | 错误码 | 是否可继续 |
|---|---|---|
| 无 `dshr-trust.json` | `E_TRUST_MISSING` | 需首装确认（未验证） |
| 摘要不匹配 | `E_INTEGRITY_MISMATCH` | 否 |
| 清单与 trust 文件不一致 | `E_TRUST_MANIFEST` | 否 |
| 有签名但密钥库无 `key_id` | `E_TRUST_UNKNOWN_KEY` | 否 |
| 密钥已吊销 | `E_TRUST_KEY_REVOKED` | 否（轮换/重签后才能装） |
| 密钥尚未生效 | `E_TRUST_KEY_NOT_YET_VALID` | 否（等时间窗） |
| 密钥已过期 | `E_TRUST_KEY_EXPIRED` | 否（换钥） |
| 签名无效 | `E_TRUST_SIGNATURE` | 否 |
| 签名有效 | —— | 可安装（无需确认，但仍审计） |

**轮换**：`chat --trust-key rotate <old_key_id> <new_key_id> <public_key_hex>` 先登记新钥、
再吊销旧钥；旧钥签名的包立即被拒，作者需用新钥重签后重新安装。旧钥保留在密钥库里
（`revoked: true` + `revoked_at_ms`）以便审计，不用 `remove` 做轮换。

**吊销/删除**：`revoke` 保留公钥与时间；`remove` 硬删除，之后按 `E_TRUST_UNKNOWN_KEY`
拒绝。安装路径按 `TrustPolicy.now_ms`（缺省系统时间）检查时间窗；直接调用
`verify_package` 的 M3 兼容路径只检查吊销、不检查时间窗。

## 5. 安装门禁顺序

```text
解析包（目录 / .tgz → 暂存目录）
  → 读包清单（package.json 或 plugin.toml）得到 name/version
  → 计算包摘要
  → 读 dshr-trust.json（若有）并校验 manifest/摘要/签名
  → 校验签名钥的吊销与时间窗（now = TrustPolicy.now_ms / 系统时间）
  → 读已安装元数据，拒绝版本降级（E_TRUST_DOWNGRADE）
  → 需要确认？（签名未验证 && 首装）→ preview + 用户确认，拒绝则 E_TRUST_NOT_CONFIRMED
  → 复制到 sidecar/node_modules/<name>（沿用 M2 的原子替换/路径安全）
  → 写 .dshr-install.json（版本/摘要/签名状态/时间）
  → 追加 install-audit.jsonl（接受与拒绝都记）
```

**卸载**（M4 W1）：`chat --uninstall-plugin <name>` 先 rename 到同父目录的
`.dshr-remove-*` 再删除（失败恢复原名），随后移除 profile 同名条目并写审计
`action: "uninstall"`。目录已删但 profile 落盘失败时返回错误，重跑即可收敛。

**首装确认**必须展示（CLI/UI 同一套字段）：包名、版本、来源路径、摘要、签名状态
（`verified` / `unsigned` / `unknown-key`）、会获得的权限（manifest `permissions`）、
"此包可在你的用户权限下执行代码"的风险提示。确认参数由调用方注入（CLI `--yes-install`、
GUI 对话框），信任层不假设交互方式。

**降级拒绝**：`<dest>/.dshr-install.json` 记录已安装版本；候选版本 `<` 已安装版本即拒绝。
显式强制（`--allow-downgrade`）时允许，但审计 `downgrade: true` 并需要确认。

**审计**：`<profile_dir>/install-audit.jsonl`，一行一条 JSON：
`{ ts, action: "install"|"reject"|"uninstall", name, version, digest_sha256, signature_status,
   key_id?, decision: "accepted"|"rejected", reason?, user_confirmed, first_install, downgrade }`。
审计不记录包内容；失败也要写（`action:"reject"`）。`uninstall` 的版本/摘要/签名状态来自
`.dshr-install.json`；元数据缺失时为空字符串/`unknown`。

**密钥库审计**：`<profile_dir>/trust-key-audit.jsonl`，一行一条：
`{ ts, action: "add"|"remove"|"revoke"|"rotate", key_id, public_key_hex,
   old_key_id?, decision: "accepted", reason? }`。密钥库变更与安装审计分开，避免把
"信任根变更"混进包安装记录。

## 6. 已安装元数据

`<dest>/.dshr-install.json`（安装后写入，卸载/覆盖时删除旧值）：

```json
{
  "schema_version": 1,
  "name": "cad.validate",
  "version": "0.1.0",
  "digest_sha256": "…",
  "signature": { "status": "verified", "key_id": "cad-vendor-2026" },
  "installed_at_ms": 1763000000000
}
```

## 7. 错误码与测试 ID

| 码 | 含义 |
|---|---|
| `E_TRUST_MISSING` | 无信任文件（允许确认后安装，标记未验证） |
| `E_TRUST_MANIFEST` | trust 文件字段/清单不一致 |
| `E_INTEGRITY_MISMATCH` | 摘要不匹配（疑似篡改） |
| `E_TRUST_UNKNOWN_KEY` | 密钥库无该 `key_id` |
| `E_TRUST_KEY_REVOKED` | 签名钥已吊销 |
| `E_TRUST_KEY_NOT_YET_VALID` | 签名钥尚未进入时间窗 |
| `E_TRUST_KEY_EXPIRED` | 签名钥已过期 |
| `E_TRUST_SIGNATURE` | 签名无效 |
| `E_TRUST_DOWNGRADE` | 版本降级且未强制 |
| `E_TRUST_NOT_CONFIRMED` | 用户/调用方拒绝确认 |

Conformance：`CPP-TRUST-001..018`（M3 摘要/签名/首装/降级/审计/元数据）、
`M4-KEY-001..010`（M4 吊销/时间窗/轮换/密钥库持久化/密钥审计）、
`M4-UNINSTALL-001..008`（M4 卸载）、`M4-LIFECYCLE-001`（chat 产品路径自检）、
`M4-SIGN-001`（作者签名打包/安装/轮换）、`M4-SDK-001`（离线 SDK 渠道）。

## 8. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| T1 | 密钥库是否支持系统证书（Windows CertStore） | 否，M4 随证书体系 | M4 |
| T2 | 包内 `.dshr-install.json` 是否参与摘要 | 否（安装后生成，属于本机元数据） | 已定 |
| T3 | 远程 registry 的信任根 | M4+，M3 只本地包 | M4 |
| T4 | 是否对 DSH JS 包做权限清单确认 | 有 `dsh` 字段时展示；无权限模型，文案明确 | M3 已按此实现 |
| T5 | 密钥轮换是否有宽限期 | 否，旧钥 `rotate` 后立即吊销；需要宽限时先保留旧钥、延迟 `revoke` | 已定（M4 W1） |
| T6 | 密钥库是否用 JSON Schema 冻结 | 是，`schema/cpp-v1/trust-keys.schema.json` + `schema/check.mjs` 正反例 | 已定（M4 W1） |
