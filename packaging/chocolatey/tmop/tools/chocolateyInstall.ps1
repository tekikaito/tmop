$ErrorActionPreference = 'Stop'

if (-not [Environment]::Is64BitOperatingSystem) {
  throw 'TMOP is only built for 64-bit Windows.'
}

$toolsDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

$packageArgs = @{
  PackageName    = $env:ChocolateyPackageName
  UnzipLocation  = $toolsDir
  Url64bit       = 'https://github.com/tekikaito/tmop/releases/download/v0.15.0/TMOP-v0.15.0-windows-x64.zip'
  Checksum64     = '80104983a79b8e4019b9bc0a0e912b0af17eb53b710ae797cc01d645b4d29f5b'
  ChecksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @packageArgs

# TMOP.exe is a GUI program: tell Chocolatey's shim not to wait for it in the console.
New-Item -ItemType File -Path (Join-Path $toolsDir 'TMOP.exe.gui') -Force | Out-Null
