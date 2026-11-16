# M4 Windows Authenticode 签名管线

G1 的签名交付需要**受信任的代码签名证书**；本机没有证书，只验证了管线本身。

## 1. 生产签名

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/sign-windows.ps1 `
  -Path dist/dshr-debug-windows-x64/chat.exe -Thumbprint <cert-thumbprint>
```

证书必须在 `Cert:\CurrentUser\My`，并且有私钥；脚本用 SHA-256 调
`Set-AuthenticodeSignature`，再 `Get-AuthenticodeSignature` 校验 signer 一致。
生产签名还应加 timestamp（当前未加，离线环境不引入外部时间戳依赖）。

## 2. 管线自检

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/sign-windows.ps1 `
  -Path <copy-of-chat.exe> -SelfSignedTest
```

实测输出：

```json
{"status":"UnknownError","signer":"CN=dshr selftest signing","ephemeral":true}
```

- `ephemeral=true` 表示一次性自签证书，签名后从 `CurrentUser\My` 删除；
- `UnknownError` 是未受信任根导致的预期状态，**不是**可发布签名；
- 脚本仍验证了 PE Authenticode 写入与签名者一致。

## 3. 未做

- 没有真实证书/时间戳/CI secrets，G1 的“签名安装包”仍未完成。
- macOS codesign/notarization、Linux 包签名未做。
- 签名后的包没有重新做安装/升级回滚验收。
