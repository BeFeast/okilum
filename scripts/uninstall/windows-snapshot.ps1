# Snapshot of the current user's profile and HKCU\Software for uninstall
# acceptance (#974). Usage: windows-snapshot.ps1 -Out before.txt
# One line per entry: "F <path>" for files/dirs under the profile, "K <key>"
# for registry keys and "V <key>\<value>" for values. Compare with diff.py.
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'SilentlyContinue'
$lines = New-Object System.Collections.Generic.List[string]
Get-ChildItem -LiteralPath $env:USERPROFILE -Recurse -Force -ErrorAction SilentlyContinue |
    ForEach-Object { $lines.Add('F ' + $_.FullName) }
Get-ChildItem -LiteralPath 'HKCU:\Software' -Recurse -ErrorAction SilentlyContinue |
    ForEach-Object {
        $key = $_.Name
        $lines.Add('K ' + $key)
        foreach ($value in $_.GetValueNames()) { $lines.Add('V ' + $key + '\' + $value) }
    }
$lines | Sort-Object -Unique | Set-Content -LiteralPath $Out -Encoding UTF8
"snapshot: $($lines.Count) entries -> $Out"
