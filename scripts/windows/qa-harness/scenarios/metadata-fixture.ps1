# Fixed data-risk fixture, not a native driver or permission to launch an app.
function NewIdentityFixture([string]$Root){
 if($Root -cnotmatch '^C:\\Users\\Public\\okilum-qa\\identity-[a-f0-9]{32}\\Identity(?:Seed|Failure)$'){throw 'Owned isolated fixture root required'}
 if(Test-Path -LiteralPath $Root){throw 'Fixture root must be new'}
 $directory=Join-Path $Root '.okilum'
 New-Item -ItemType Directory -Path $directory -ErrorAction Stop|Out-Null
 $text='{ "future" : {"revision":23,"flags":[true,null,"שלום"]},'+"`r`n"+'  "name" : "QA Ελληνικά" }'+"`r`n"
 [IO.File]::WriteAllText((Join-Path $directory 'vault.json'),$text,(New-Object Text.UTF8Encoding($false)))
 [IO.File]::WriteAllText((Join-Path $Root 'Home.md'),"# Home`nOwned identity fixture`n",(New-Object Text.UTF8Encoding($false)))
 [IO.File]::WriteAllText((Join-Path $Root 'Landing.md'),"# Landing`nPositive navigation control`n",(New-Object Text.UTF8Encoding($false)))
}
function EnterIdentityWriteFailure([string]$Metadata){
 if($Metadata -cnotmatch '^C:\\Users\\Public\\okilum-qa\\identity-[a-f0-9]{32}\\IdentityFailure\\.okilum\\vault.json$'){throw 'Foreign metadata fixture refused'}
 # Windows sharing semantics: allow readers, deny writes and atomic replacement.
 # Keep this handle in the SAME desktop step as real Copy; dispose in finally.
 $handle=[IO.File]::Open($Metadata,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
 try{
  $bytes=[IO.File]::ReadAllBytes($Metadata)
  $writeDenied=$false;$replaceDenied=$false
  try{$probe=[IO.File]::Open($Metadata,[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::ReadWrite);$probe.Dispose()}catch [IO.IOException]{$writeDenied=$true}
  # Opening DELETE access proves the atomic-rename boundary without replacing bytes.
  if(-not ('IdentityNative' -as [type])){
   Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class IdentityNative {
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
 public static extern IntPtr CreateFile(string name,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
 [DllImport("kernel32.dll", SetLastError=true)] public static extern bool CloseHandle(IntPtr handle);
}
'@
  }
  $native=[IdentityNative]::CreateFile($Metadata,0x10000,7,[IntPtr]::Zero,3,0,[IntPtr]::Zero)
  if($native -eq [IntPtr](-1)){
   $code=[Runtime.InteropServices.Marshal]::GetLastWin32Error()
   if($code -ne 32){throw ('Expected sharing violation, got '+$code)}
   $replaceDenied=$true
  }else{[void][IdentityNative]::CloseHandle($native)}
  if(!$writeDenied -or !$replaceDenied){throw 'Write-failure positive control did not fire'}
  return [pscustomobject]@{handle=$handle;before=$bytes;probe=@{read_succeeded=$true;write_denied=$writeDenied;replace_denied=$replaceDenied}}
 }catch{$handle.Dispose();throw}
}
