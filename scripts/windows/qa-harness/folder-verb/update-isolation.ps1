# Snapshot-local PR gate isolation; rollback restores the original hosts file.
$ErrorActionPreference='Stop'
function BlockPrUpdateFeed($Evidence){
 $hostsPath=Join-Path $env:windir 'System32/drivers/etc/hosts'
 Copy-Item -LiteralPath $hostsPath -Destination (Join-Path $Evidence 'hosts-before.txt')
 $before=[Net.Dns]::GetHostAddresses('updates.befeast.com')|ForEach-Object {$_.ToString()}
 if(!@($before|Where-Object {$_ -notin @('0.0.0.0','127.0.0.1','::1','::')}).Count){throw 'Feed already blocked before positive resolver control'}
 Add-Content -LiteralPath $hostsPath -Value "`r`n0.0.0.0 updates.befeast.com # Okilum PR gate only`r`n::1 updates.befeast.com # Okilum PR gate only" -Encoding ASCII
 Clear-DnsClientCache
 $after=@([Net.Dns]::GetHostAddresses('updates.befeast.com')|ForEach-Object {$_.ToString()})
 if(!$after.Count -or @($after|Where-Object {$_ -notin @('0.0.0.0','::1')}).Count){throw 'PR update feed isolation not effective'}
 @{host='updates.befeast.com';before=$before;after=$after;restoration='canonical rollback'}|ConvertTo-Json|Set-Content (Join-Path $Evidence 'update-isolation.json')
}
