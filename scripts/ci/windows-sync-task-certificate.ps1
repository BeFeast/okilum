param(
    [Parameter(Mandatory=$true)][ValidateSet('create','sign','cleanup')][string]$Action,
    [Parameter(Mandatory=$true)][string]$Metadata,
    [string]$File
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Native signing fixtures run only on a disposable hosted Windows runner'
}
$principal = [System.Security.Principal.WindowsPrincipal]::new([System.Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Disposable machine-root fixture requires the hosted runner elevated token'
}
Write-Host "Native fixture certificate phase: $Action"
# Use PowerShell 7's .NET certificate API. Windows PowerShell's PKI/provider
# autoload inherited from pwsh stalled before returning a fixture certificate.
function Persist-Certificate($certificate) {
    $bytes = $certificate.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Pfx, '')
    try {
        $flags = [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::UserKeySet -bor
                 [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::PersistKeySet
        return [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($bytes, '', $flags)
    } finally { [Array]::Clear($bytes, 0, $bytes.Length) }
}
function Open-Store([string]$name) {
    $store = [System.Security.Cryptography.X509Certificates.X509Store]::new($name, 'CurrentUser')
    $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
    return $store
}
function Certificate([string]$thumbprint) {
    $store = Open-Store 'My'
    try {
        $found = $store.Certificates.Find('FindByThumbprint', $thumbprint, $false)
        if ($found.Count -ne 1) { throw 'Fixture certificate missing or ambiguous' }
        return $found[0]
    } finally { $store.Close() }
}
if ($Action -eq 'create') {
    if (Test-Path -LiteralPath $Metadata) { throw 'Fixture certificate metadata already exists' }
    $id = [Guid]::NewGuid().ToString('N')
    $start = [DateTimeOffset]::Now.AddMinutes(-5)
    $rootKey = [System.Security.Cryptography.RSA]::Create(2048)
    $leafKey = [System.Security.Cryptography.RSA]::Create(2048)
    try {
        $rootRequest = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
            "CN=Okilum native fixture root $id", $rootKey,
            [System.Security.Cryptography.HashAlgorithmName]::SHA256, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $rootRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($true, $false, 0, $true))
        $usage = [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyCertSign -bor
                 [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::CrlSign
        $rootRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new($usage, $true))
        $rootRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509SubjectKeyIdentifierExtension]::new($rootRequest.PublicKey, $false))
        $root = $rootRequest.CreateSelfSigned($start, [DateTimeOffset]::Now.AddHours(2))
        Write-Host 'Native fixture root created'
        $leafRequest = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
            "CN=Okilum native fixture publisher $id", $leafKey,
            [System.Security.Cryptography.HashAlgorithmName]::SHA256, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
        Write-Host 'Native fixture leaf request created'
        $leafRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($false, $false, 0, $true))
        $leafRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
            [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature, $true))
        $purposes = [System.Security.Cryptography.OidCollection]::new()
        [void]$purposes.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.3'))
        $leafRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($purposes, $true))
        Write-Host 'Native fixture code-signing purpose encoded'
        $leafRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509SubjectKeyIdentifierExtension]::new($leafRequest.PublicKey, $false))
        Write-Host 'Native fixture leaf request prepared'
        # The issuer is an ephemeral key already held in this process. Avoid
        # exporting/reimporting it and then rediscovering it through a provider.
        $signer = [System.Security.Cryptography.X509Certificates.X509SignatureGenerator]::CreateForRSA(
            $rootKey, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $issued = $leafRequest.Create($root.SubjectName, $signer, $start,
                                      [DateTimeOffset]::Now.AddHours(1), [Guid]::NewGuid().ToByteArray())
        Write-Host 'Native fixture leaf certificate issued'
        $withKey = [System.Security.Cryptography.X509Certificates.RSACertificateExtensions]::CopyWithPrivateKey($issued, $leafKey)
        $leaf = Persist-Certificate $withKey
        Write-Host 'Native fixture leaf private key persisted'
        $mine = Open-Store 'My'
        try { $mine.Add($leaf) } finally { $mine.Close() }
        Write-Host 'Native fixture leaf stored'
        # CurrentUser Root requires a trust confirmation even with certutil -f
        # (force overwrite). The disposable hosted VM has an elevated runner;
        # install exactly this temporary public root in its machine store.
        $publicRoot = [System.IO.Path]::ChangeExtension($Metadata, 'root.cer')
        [System.IO.File]::WriteAllBytes($publicRoot, $root.RawData)
        & "$env:SystemRoot\System32\certutil.exe" -f -addstore Root $publicRoot
        if ($LASTEXITCODE -ne 0) { throw 'Installing the disposable fixture root failed' }
        [ordered]@{ schema=1; trust_store='LocalMachine'; root=$root.Thumbprint; leaf=$leaf.Thumbprint; subject=$leaf.Subject; issuers=@($root.Subject) } |
            ConvertTo-Json -Compress | Set-Content -LiteralPath $Metadata -Encoding UTF8
        Write-Host 'Native fixture user leaf and disposable machine-root trust installed'
    } finally { $rootKey.Dispose(); $leafKey.Dispose() }
    exit 0
}
$record = Get-Content -Raw -LiteralPath $Metadata | ConvertFrom-Json
if ($record.schema -ne 1 -or $record.trust_store -ne 'LocalMachine' -or $record.root -notmatch '^[A-F0-9]{40}$' -or $record.leaf -notmatch '^[A-F0-9]{40}$') {
    throw 'Invalid fixture certificate identity'
}
if ($Action -eq 'sign') {
    $certificate = Certificate $record.leaf
    if ($certificate.Subject -ne $record.subject) { throw 'Fixture certificate subject changed' }
    $result = Set-AuthenticodeSignature -FilePath $File -Certificate $certificate -HashAlgorithm SHA256 -IncludeChain All
    if ($result.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Fixture signature failed: $($result.Status) $($result.StatusMessage)"
    }
    Write-Host 'Native fixture embedded signature verified'
    exit 0
}
& "$env:SystemRoot\System32\certutil.exe" -f -delstore Root $record.root
if ($LASTEXITCODE -ne 0) { throw 'Removing the disposable fixture root failed' }
$mine = Open-Store 'My'
try {
    foreach ($thumbprint in @($record.leaf)) {
        foreach ($certificate in $mine.Certificates.Find('FindByThumbprint', $thumbprint, $false)) {
            if ($certificate.Subject -notlike 'CN=Okilum native fixture *') { throw 'Refusing to remove a non-fixture key' }
            $key = [System.Security.Cryptography.X509Certificates.RSACertificateExtensions]::GetRSAPrivateKey($certificate)
            try {
                if ($key -is [System.Security.Cryptography.RSACng]) {
                    $key.Key.Delete()
                } elseif ($key -is [System.Security.Cryptography.RSACryptoServiceProvider]) {
                    $key.PersistKeyInCsp = $false
                    $key.Clear()
                } else { throw 'Fixture private-key provider cannot be cleaned up' }
            } finally { if ($key) { $key.Dispose() } }
            $mine.Remove($certificate)
        }
    }
} finally { $mine.Close() }
Write-Host 'Native fixture trust and private keys removed'
