param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Thumbprint
)

$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw 'A numeric three-part version is required.' }
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$certificate = Get-Item -LiteralPath ("Cert:\CurrentUser\My\" + $Thumbprint) -ErrorAction Stop
if (-not $certificate.HasPrivateKey) { throw 'The publishing certificate has no private key.' }
$rsa = [System.Security.Cryptography.X509Certificates.RSACertificateExtensions]::GetRSAPrivateKey($certificate)
if ($null -eq $rsa -or $rsa.KeySize -ne 3072) { throw 'The publisher key must be RSA-3072.' }
$public = [System.Security.Cryptography.X509Certificates.RSACertificateExtensions]::GetRSAPublicKey($certificate)
$p = $public.ExportParameters($false)
$blob = New-Object byte[] (24 + $p.Exponent.Length + $p.Modulus.Length)
[BitConverter]::GetBytes([uint32]0x31415352).CopyTo($blob, 0)
[BitConverter]::GetBytes([uint32]($p.Modulus.Length * 8)).CopyTo($blob, 4)
[BitConverter]::GetBytes([uint32]$p.Exponent.Length).CopyTo($blob, 8)
[BitConverter]::GetBytes([uint32]$p.Modulus.Length).CopyTo($blob, 12)
$p.Exponent.CopyTo($blob, 24)
$p.Modulus.CopyTo($blob, 24 + $p.Exponent.Length)
$pinned = [IO.File]::ReadAllBytes((Join-Path $PSScriptRoot '..\resources\update-public-key.blob'))
if ([Convert]::ToBase64String($blob) -ne [Convert]::ToBase64String($pinned)) {
    throw 'The publisher key does not match the public key pinned in the application.'
}
$stream = [IO.File]::OpenRead($installerPath)
$sha = [Security.Cryptography.SHA256]::Create()
try { $digest = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
finally { $sha.Dispose(); $stream.Dispose() }
$message = [Text.Encoding]::UTF8.GetBytes("isolmaSS-update-v1`n$Version`n$digest`n")
$signature = $rsa.SignData($message, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
if ($signature.Length -ne 384 -or -not $public.VerifyData($message, $signature, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)) {
    throw 'The generated update signature failed verification.'
}
$destination = "$installerPath.sig"
$temporary = "$destination.$PID.tmp"
try {
    [IO.File]::WriteAllBytes($temporary, $signature)
    Move-Item -LiteralPath $temporary -Destination $destination -Force
} finally {
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
}
Write-Output "Signed v$Version ($digest); detached signature: $destination"
