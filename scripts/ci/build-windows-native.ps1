# Unsigned PR packaging only. Publication and signing remain on local runners.
$ErrorActionPreference = 'Stop'
function Checked([scriptblock] $Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "Command failed ($LASTEXITCODE): $Command" }
}
$env:OKILUM_SOURCE_COMMIT = (git rev-parse HEAD).Trim()
$env:OKILUM_BUILD_VERSION = [string](5000 + [int]$env:GITHUB_RUN_NUMBER)
$env:OKILUM_RELEASE_VERSION = "0.1.$env:OKILUM_BUILD_VERSION"
$payload = Join-Path $env:GITHUB_WORKSPACE 'target/windows-dist'
$env:OKILUM_WINDOWS_ICON = Join-Path $payload 'okilum.ico'
if (!(Test-Path $env:OKILUM_WINDOWS_ICON)) { throw 'Approved icon artifact is missing' }
$env:RUSTFLAGS = '-C target-feature=+crt-static'
# GPUI runtime shaders must resolve inside the installed payload, not D:\a\...
$wrapper = Join-Path $env:RUNNER_TEMP 'okilum-rustc-wrapper.exe'
Checked { rustc --edition=2021 scripts/ci/windows-rustc.rs -o $wrapper }
# Regression control: cmd.exe used to split quoted --check-cfg values.
$probe = Join-Path $env:RUNNER_TEMP 'okilum-wrapper-probe.rs'
'fn main() {}' | Set-Content $probe
$probeExe = Join-Path $env:RUNNER_TEMP 'okilum-wrapper-probe.exe'
Checked { & $wrapper (Get-Command rustc).Source $probe --crate-name wrapper_probe --check-cfg 'cfg(feature, values("a b", "c"))' -o $probeExe }
if (!(Test-Path $probeExe)) { throw 'Wrapper argument-preservation positive control failed' }
$env:RUSTC_WRAPPER = $wrapper
Checked { rustc -Vv }
Checked { bash scripts/vendor-setup.sh }
Checked { bash scripts/vendor-setup.sh --verify }
Checked { cargo build --locked --target x86_64-pc-windows-msvc --profile windows-diagnostic -p okilum-shell --no-default-features }
Copy-Item target/x86_64-pc-windows-msvc/windows-diagnostic/okilum.exe $payload
Checked { python scripts/third-party-notices.py --stage $payload }
Copy-Item docs/windows-delivery.md "$payload/README.md"
Checked { python scripts/ci/stage-windows-shaders.py $payload }
$tools = Join-Path $env:RUNNER_TEMP 'okilum-vpk'
New-Item -ItemType Directory -Force $tools | Out-Null
$archive = Join-Path $tools 'vpk.zip'
Invoke-WebRequest 'https://github.com/velopack/velopack/releases/download/1.2.161/vpk.1.2.161.nupkg' -OutFile $archive
if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLower() -ne '2b56ce117f803fc70c103cb423bd040e395e40370f6ff6e10818e9ff9c26a323') {
    throw 'Pinned Velopack archive checksum mismatch'
}
Expand-Archive $archive -DestinationPath $tools -Force
$channel = (Get-Content scripts/windows/channel.json | ConvertFrom-Json).default_channel
Checked { dotnet "$tools/tools/net8.0/any/vpk.dll" pack --packId BeFeast.Okilum --packTitle Okilum --packAuthors BeFeast --packVersion $env:OKILUM_RELEASE_VERSION --packDir $payload --mainExe okilum.exe --runtime win-x64 --channel $channel --icon $env:OKILUM_WINDOWS_ICON --exclude '.*\.(pdb|zip|sha256)$|metadata\.json' --outputDir target/windows-release --skip-updates --yes }
if (!(Get-ChildItem target/windows-release -Filter '*.nupkg')) { throw 'Packager produced no package' }
