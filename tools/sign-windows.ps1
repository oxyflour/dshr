param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [string]$Thumbprint = "",
    [switch]$SelfSignedTest
)

# M4 G1 签名管线（Windows Authenticode）。
# 生产签名：传入 -Thumbprint（CurrentUser\My 中的代码签名证书）。
# 仅验证管线：-SelfSignedTest 生成一次性自签证书，不写证书库、不 timestamp；
# 结果一定不是受信任发布签名，不能当作 G1 签名交付。

$ErrorActionPreference = "Stop"
$resolved = (Resolve-Path $Path).Path
if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
    throw "missing file: $resolved"
}

$certificate = $null
$ephemeral = $false
if ($Thumbprint) {
    $certificate = Get-ChildItem Cert:\CurrentUser\My |
        Where-Object { $_.Thumbprint -eq $Thumbprint } |
        Select-Object -First 1
    if (-not $certificate) { throw "certificate not found in CurrentUser\My: $Thumbprint" }
} elseif ($SelfSignedTest) {
    $certificate = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject "CN=dshr selftest signing" `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -KeyExportPolicy Exportable `
        -NotAfter (Get-Date).AddDays(1)
    $ephemeral = $true
} else {
    throw "provide -Thumbprint (production) or -SelfSignedTest (pipeline check)"
}

$signature = Set-AuthenticodeSignature -FilePath $resolved -Certificate $certificate -HashAlgorithm SHA256
if (-not $signature.SignerCertificate) {
    throw "Authenticode signing produced no signer certificate"
}
$check = Get-AuthenticodeSignature -LiteralPath $resolved
[pscustomobject]@{
    path = $resolved
    status = $check.Status.ToString()
    signer = $check.SignerCertificate.Subject
    ephemeral = $ephemeral
} | ConvertTo-Json -Compress
if ($check.SignerCertificate.Subject -ne $signature.SignerCertificate.Subject) {
    throw "signer mismatch after signing"
}
if ($ephemeral) {
    Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($certificate.Thumbprint)" -Force -ErrorAction SilentlyContinue
}
