# Unsigned PR packaging only. Publication and signing remain on local runners.
$ErrorActionPreference = 'Stop'
function Checked([scriptblock] $Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "Command failed ($LASTEXITCODE): $Command" }
}
$env:TESSERA_SOURCE_COMMIT = (git rev-parse HEAD).Trim()
$env:TESSERA_BUILD_VERSION = [string](5000 + [int]$env:GITHUB_RUN_NUMBER)
$env:TESSERA_RELEASE_VERSION = "0.1.$env:TESSERA_BUILD_VERSION"
$payload = Join-Path $env:GITHUB_WORKSPACE 'target/windows-dist'
$env:TESSERA_WINDOWS_ICON = Join-Path $payload 'tessera.ico'
if (!(Test-Path $env:TESSERA_WINDOWS_ICON)) { throw 'Approved icon artifact is missing' }
$env:RUSTFLAGS = '-C target-feature=+crt-static'
# GPUI runtime shaders must resolve inside the installed payload, not D:\a\...
$env:RUSTC_WRAPPER = Join-Path $env:GITHUB_WORKSPACE 'scripts/ci/windows-rustc.cmd'
Checked { rustc -Vv }
Checked { bash scripts/vendor-setup.sh }
Checked { bash scripts/vendor-setup.sh --verify }
Checked { cargo build --locked --target x86_64-pc-windows-msvc --profile windows-diagnostic -p tessera-shell --no-default-features }
Copy-Item target/x86_64-pc-windows-msvc/windows-diagnostic/tessera.exe $payload
Checked { python scripts/third-party-notices.py --stage $payload }
Copy-Item docs/windows-delivery.md "$payload/README.md"
Checked { python scripts/ci/stage-windows-shaders.py $payload }
$tools = Join-Path $env:RUNNER_TEMP 'tessera-vpk'
New-Item -ItemType Directory -Force $tools | Out-Null
$archive = Join-Path $tools 'vpk.zip'
Invoke-WebRequest 'https://github.com/velopack/velopack/releases/download/1.2.161/vpk.1.2.161.nupkg' -OutFile $archive
if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLower() -ne '2b56ce117f803fc70c103cb423bd040e395e40370f6ff6e10818e9ff9c26a323') {
    throw 'Pinned Velopack archive checksum mismatch'
}
Expand-Archive $archive -DestinationPath $tools -Force
$channel = (Get-Content scripts/windows/channel.json | ConvertFrom-Json).default_channel
Checked { dotnet "$tools/tools/net8.0/any/vpk.dll" pack --packId BeFeast.Tessera --packTitle Tessera --packAuthors BeFeast --packVersion $env:TESSERA_RELEASE_VERSION --packDir $payload --mainExe tessera.exe --runtime win-x64 --channel $channel --icon $env:TESSERA_WINDOWS_ICON --exclude '.*\.(pdb|zip|sha256)$|metadata\.json' --outputDir target/windows-release --skip-updates --yes }
if (!(Get-ChildItem target/windows-release -Filter '*.nupkg')) { throw 'Packager produced no package' }
