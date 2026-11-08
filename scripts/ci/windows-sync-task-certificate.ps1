param(
    [Parameter(Mandatory=$true)][ValidateSet('create','sign','cleanup')][string]$Action,
    [Parameter(Mandatory=$true)][string]$Metadata,
    [string]$File
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Native signing fixtures run only on a disposable hosted Windows runner'
}
# This is launched from pwsh by Python into Windows PowerShell. Load its own
# security provider explicitly rather than inheriting pwsh's module search path.
Import-Module "$PSHOME\Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1" -Force
if (-not (Get-PSDrive -Name Cert -ErrorAction SilentlyContinue)) {
    New-PSDrive -Name Cert -PSProvider Certificate -Root '\' | Out-Null
}
if ($Action -eq 'create') {
    if (Test-Path -LiteralPath $Metadata) { throw 'Fixture certificate metadata already exists' }
    $id = [Guid]::NewGuid().ToString('N')
    $root = New-SelfSignedCertificate -Type Custom -Subject "CN=Okilum native fixture root $id" `
        -KeyUsage CertSign,CRLSign,DigitalSignature -TextExtension @('2.5.29.19={critical}{text}ca=true') `
        -CertStoreLocation 'Cert:\CurrentUser\My' -NotAfter (Get-Date).AddHours(2) `
        -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256
    $leaf = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=Okilum native fixture publisher $id" `
        -Signer $root -CertStoreLocation 'Cert:\CurrentUser\My' -NotAfter (Get-Date).AddHours(1) `
        -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store('Root','CurrentUser')
    $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
    try { $store.Add($root) } finally { $store.Close() }
    [ordered]@{ schema=1; root=$root.Thumbprint; leaf=$leaf.Thumbprint; subject=$leaf.Subject; issuers=@($root.Subject) } |
        ConvertTo-Json -Compress | Set-Content -LiteralPath $Metadata -Encoding UTF8
    exit 0
}
$record = Get-Content -Raw -LiteralPath $Metadata | ConvertFrom-Json
if ($record.schema -ne 1 -or $record.root -notmatch '^[A-F0-9]{40}$' -or $record.leaf -notmatch '^[A-F0-9]{40}$') {
    throw 'Invalid fixture certificate identity'
}
if ($Action -eq 'sign') {
    $certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$($record.leaf)"
    if ($certificate.Subject -ne $record.subject) { throw 'Fixture certificate subject changed' }
    $result = Set-AuthenticodeSignature -FilePath $File -Certificate $certificate -HashAlgorithm SHA256 -IncludeChain All
    if ($result.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Fixture signature failed: $($result.Status) $($result.StatusMessage)"
    }
    exit 0
}
foreach ($path in @("Cert:\CurrentUser\Root\$($record.root)", "Cert:\CurrentUser\My\$($record.leaf)", "Cert:\CurrentUser\My\$($record.root)")) {
    if (Test-Path -LiteralPath $path) {
        if ($path.StartsWith('Cert:\CurrentUser\Root\')) { Remove-Item -LiteralPath $path }
        else { Remove-Item -LiteralPath $path -DeleteKey }
    }
}
