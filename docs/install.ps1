# Installs or updates TMOP for the current user. Run it in PowerShell:
#
#   irm https://tekikaito.github.io/tmop/install.ps1 | iex
#
# It downloads the newest TMOP.exe for this PC's processor from GitHub, checks its SHA-256, puts it
# in %LOCALAPPDATA%\Programs\TMOP, adds a Start menu entry and an entry in Settings > Apps (which
# removes it again), then starts TMOP. No administrator permission is needed.
#
# Everything sits in a function so that piping into iex never runs `exit` in the user's window.

function Install-TMOP {
    $ErrorActionPreference = 'Stop'
    $repo = 'tekikaito/tmop'
    $dir = Join-Path $env:LOCALAPPDATA 'Programs\TMOP'
    $exe = Join-Path $dir 'TMOP.exe'
    $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'TMOP.lnk'
    $uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TMOP'

    # Windows PowerShell 5.1 may still default to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    # The native architecture: PROCESSOR_ARCHITECTURE of this process says AMD64 when x64
    # PowerShell runs emulated on an ARM PC, the machine-wide value does not.
    $native = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment').PROCESSOR_ARCHITECTURE
    $arch = if ($native -eq 'ARM64') { 'arm64' } elseif ($native -eq 'AMD64') { 'x64' } else { $null }
    if (-not $arch) {
        Write-Host "TMOP needs 64-bit Windows (x64 or ARM64); this PC reports '$native'." -ForegroundColor Red
        return
    }

    Write-Host "Looking up the newest TMOP release..."
    $release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'TMOP-installer' }
    $version = $release.tag_name -replace '^v', ''
    # Releases since the download page carry the bare exe; older ones only the zip.
    $asset = $release.assets | Where-Object { $_.name -eq "TMOP-windows-$arch.exe" } | Select-Object -First 1
    if (-not $asset) {
        $asset = $release.assets | Where-Object { $_.name -like "TMOP-v*-windows-$arch.zip" } | Select-Object -First 1
    }
    $sums = $release.assets | Where-Object { $_.name -eq "$($asset.name).sha256" } | Select-Object -First 1
    if (-not $asset -or -not $sums) {
        Write-Host "Release $($release.tag_name) has no TMOP download for $arch." -ForegroundColor Red
        return
    }

    $running = Get-Process -Name TMOP -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }
    if ($running) {
        Write-Host "TMOP is running. Close it (File > Exit) and run this command again." -ForegroundColor Yellow
        return
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("TMOP-install-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    $progress = $ProgressPreference
    try {
        # The progress bar makes Invoke-WebRequest many times slower in Windows PowerShell 5.1.
        $ProgressPreference = 'SilentlyContinue'
        Write-Host "Downloading TMOP $version ($arch)..."
        $download = Join-Path $tmp $asset.name
        Invoke-WebRequest $asset.browser_download_url -OutFile $download -UseBasicParsing
        $sumsFile = Join-Path $tmp $sums.name
        Invoke-WebRequest $sums.browser_download_url -OutFile $sumsFile -UseBasicParsing
        $expected = ((Get-Content $sumsFile -Raw).Trim() -split '\s+')[0]
        $actual = (Get-FileHash $download -Algorithm SHA256).Hash
        if ($actual -ne $expected) {
            Write-Host "The download is damaged (SHA-256 does not match). Nothing was installed; please try again." -ForegroundColor Red
            return
        }

        $new = $download
        if ($asset.name -like '*.zip') {
            Expand-Archive $download -DestinationPath (Join-Path $tmp 'zip')
            $new = Join-Path $tmp 'zip\TMOP.exe'
        }
        $existed = Test-Path $exe
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        Copy-Item $new $exe -Force
    } finally {
        $ProgressPreference = $progress
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = $exe
    $link.WorkingDirectory = $dir
    $link.Description = 'TMOP - Task Manager OverPowered'
    $link.Save()

    # Settings > Apps runs this to remove TMOP again.
    $uninstall = Join-Path $dir 'uninstall.ps1'
    Set-Content -Path $uninstall -Encoding UTF8 -Value @'
$ErrorActionPreference = 'Stop'
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $dir 'TMOP.exe'
Get-Process -Name TMOP -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Start-Sleep -Milliseconds 500

# If this copy opens instead of Task Manager, hand that back to Windows first, or
# Ctrl+Shift+Esc would open nothing once TMOP.exe is gone.
$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe'
$debugger = (Get-ItemProperty $ifeo -Name Debugger -ErrorAction SilentlyContinue).Debugger
if ($debugger -and $debugger.Trim('"') -like "$exe*") {
    $p = Start-Process $exe -ArgumentList '--restore-taskmgr' -Wait -PassThru
    if ($p.ExitCode -ne 0) {
        Write-Host 'Windows Task Manager could not be restored, so TMOP was not removed.' -ForegroundColor Red
        Read-Host 'Press Enter to close'
        return
    }
}

Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'TMOP.lnk') -Force -ErrorAction SilentlyContinue
Remove-Item 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TMOP' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item $dir -Recurse -Force
Write-Host 'TMOP is removed. Your settings stay in %LOCALAPPDATA%\TMOP; delete that folder to remove them too.'
'@

    New-Item -Path $uninstallKey -Force | Out-Null
    $command = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$uninstall`""
    $values = @{
        DisplayName     = 'TMOP - Task Manager OverPowered'
        DisplayVersion  = $version
        DisplayIcon     = $exe
        Publisher       = 'tekikaito'
        InstallLocation = $dir
        URLInfoAbout    = "https://github.com/$repo"
        UninstallString = $command
    }
    foreach ($name in $values.Keys) {
        New-ItemProperty -Path $uninstallKey -Name $name -Value $values[$name] -PropertyType String -Force | Out-Null
    }
    foreach ($name in 'NoModify', 'NoRepair') {
        New-ItemProperty -Path $uninstallKey -Name $name -Value 1 -PropertyType DWord -Force | Out-Null
    }
    $sizeKb = [int]((Get-Item $exe).Length / 1KB)
    New-ItemProperty -Path $uninstallKey -Name EstimatedSize -Value $sizeKb -PropertyType DWord -Force | Out-Null

    if ($existed) {
        Write-Host "TMOP is updated to $version." -ForegroundColor Green
    } else {
        Write-Host "TMOP $version is installed. Find it in the Start menu as TMOP." -ForegroundColor Green
    }
    Start-Process $exe
}

Install-TMOP
