# Okilum uninstall, the part that must wait for Velopack's Update.exe to exit (#974).
# app_footprint::uninstall prepends $Root (the install root) and $Local (%LOCALAPPDATA%).
#
# Velopack appends to its per-app log after the hook, and Explorer records the
# shortcuts and executables while they are removed, so this runs once Update.exe
# is gone. An interactive uninstall waits on its "complete" dialog; give it an hour.
for ($i = 0; $i -lt 3600; $i++) {
    if (-not (Get-Process Update -EA SilentlyContinue | Where-Object { $_.Path -like "$Root\*" })) { break }
    Start-Sleep 1
}
Start-Sleep 2

$folder = Join-Path $Local 'velopack'
Remove-Item -LiteralPath (Join-Path $folder 'velopack_BeFeast.Okilum.log') -Force -EA SilentlyContinue
if (-not (Get-ChildItem -LiteralPath $folder -Force -EA SilentlyContinue)) {
    Remove-Item -LiteralPath $folder -Force -EA SilentlyContinue
}

# What Windows keeps about Okilum's executables, shortcuts and links. Each value
# is matched by our install root, our app ids or our link and Markdown names, so
# nothing that belongs to another program is touched.
$ids = @('velopack.BeFeast.Okilum', 'com.befeast.okilum')
function Test-Ours([string]$name) {
    # Paths may be spelled with a known-folder GUID instead of %LOCALAPPDATA%.
    $name -like "$Root\*" -or $name -like '*\BeFeast.Okilum\*' -or $ids -contains $name -or
        $name -like 'BeFeast.Okilum.*' -or $name -like '*_okilum'
}
function Remove-Values([string]$key, [scriptblock]$decode = { param($n) $n }) {
    $item = Get-Item -LiteralPath $key -EA SilentlyContinue
    if (-not $item) { return }
    foreach ($name in $item.GetValueNames()) {
        if (Test-Ours (& $decode $name)) { Remove-ItemProperty -LiteralPath $key -Name $name -Force -EA SilentlyContinue }
    }
}
Remove-Values 'HKCU:\Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache' {
    param($n) $n -replace '\.(FriendlyAppName|ApplicationCompany)$', ''
}
Remove-Values 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Compatibility Assistant\Store'
Remove-Values 'HKCU:\Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts'
Remove-Values 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Search\JumplistData'
foreach ($id in $ids) {
    Remove-Item -LiteralPath "HKCU:\Software\Microsoft\Windows\CurrentVersion\Start\TileProperties\W~$id" -Recurse -Force -EA SilentlyContinue
}
# UserAssist value names are ROT13 of the executable path or app id.
function ConvertFrom-Rot13([string]$text) {
    -join ($text.ToCharArray() | ForEach-Object {
        $c = [int]$_
        if ($c -ge 65 -and $c -le 90) { [char](65 + ($c - 65 + 13) % 26) }
        elseif ($c -ge 97 -and $c -le 122) { [char](97 + ($c - 97 + 13) % 26) }
        else { $_ }
    })
}
foreach ($count in Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist' -EA SilentlyContinue) {
    Remove-Values (Join-Path $count.PSPath 'Count') { param($n) ConvertFrom-Rot13 $n }
}
