<#
Measures what TMOP itself costs on one page (#194). Windows PowerShell 5.1 or later, no extra tools.

Starts the given TMOP.exe once, on -Page (or -Popout), with a private, empty settings folder
(LOCALAPPDATA points at a new temporary folder, so the user's settings are never read or written)
and TMOP_PROFILE set, so TMOP appends its own frame and sampler figures to a file there. The window
opens without taking the focus. After -WarmUp seconds it samples once a second for -Seconds:

  * TMOP's CPU time (Get-Process, by PID) as % of one core and % of the whole machine;
  * TMOP's GPU use: \GPU Engine(pid_<pid>_*)\Utilization Percentage, the busiest engine type
    (as Task Manager shows it) and the 3D engines alone; GPU memory from \GPU Process Memory;
  * working set, private bytes, handles and threads;
  * the machine's own load (GetSystemTimes, the sum of all 3D engines) for context.

Then it ends exactly the process it started (through its own handle), removes the temporary folder
(unless -Keep) and prints one result object; -Csv appends it to a CSV file.

  .\scripts\measure.ps1 -Page performance
  .\scripts\measure.ps1 -Popout -Seconds 30
  .\scripts\measure.ps1 -Page summary -Window minimized    # TMOP_PROFILE_WINDOW, see src/ui/mod.rs
  .\scripts\measure.ps1 -Label no-bloom -Settings '{"version":1,"appearance":{"bloom":false}}'
  foreach ($p in 'summary','performance','processes') { .\scripts\measure.ps1 -Page $p -Csv perf.csv }

The frame figures (fps, frame_ms, ui_thread_pct, sampler_pct, ..._ms) come from TMOP's profile
lines; while the window is minimized eframe draws no frames, so there are none and they read NaN.
The numbers are noisy on a machine in use: take several runs and compare spreads, not single values.
#>
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\TMOP.exe'),
    [string]$Page = 'summary',
    [string]$Device,
    [switch]$Popout,
    [ValidateSet('normal', 'minimized', 'hidden')][string]$Window = 'normal',
    [int]$WarmUp = 10,
    [int]$Seconds = 40,
    [string]$Size = '1320x860',
    [string]$Settings,
    [string]$Label,
    [string]$Csv,
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
$home_dir = Join-Path ([IO.Path]::GetTempPath()) ('tmop-measure-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $home_dir | Out-Null
$profile_file = Join-Path $home_dir 'profile.txt'
if ($Settings) {
    # A settings file for the private folder only; missing keys keep their defaults.
    New-Item -ItemType Directory -Path (Join-Path $home_dir 'TMOP') | Out-Null
    [IO.File]::WriteAllText((Join-Path $home_dir 'TMOP\settings.json'), $Settings, (New-Object Text.UTF8Encoding $false))
}

$tmop_args = if ($Popout) { @('--popout') } else { @("--page=$Page", "--window-size=$Size") }
if ($Device -and -not $Popout) { $tmop_args += "--device=$Device" }
$saved = @{}
foreach ($k in 'LOCALAPPDATA', 'TMOP_PROFILE', 'TMOP_PROFILE_WINDOW') { $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process') }
$env:LOCALAPPDATA = $home_dir
$env:TMOP_PROFILE = $profile_file
$env:TMOP_PROFILE_WINDOW = $Window
try {
    $proc = Start-Process -FilePath $Exe -ArgumentList $tmop_args -PassThru
} catch {
    # Nothing started: leave no temporary folder behind.
    Remove-Item -Recurse -Force $home_dir -ErrorAction SilentlyContinue
    throw
} finally {
    foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }
}
$id = $proc.Id
Write-Host "started TMOP pid $id ($($tmop_args -join ' '), window $Window)"

$cores = [Environment]::ProcessorCount
# The machine's CPU load from GetSystemTimes: counter names such as \Processor(_Total) are
# translated on non-English Windows, the GPU ones are not.
if (-not ('TmopMeasure.SysTimes' -as [type])) {
    Add-Type -Namespace TmopMeasure -Name SysTimes -MemberDefinition '[DllImport("kernel32.dll")] public static extern bool GetSystemTimes(out long idle, out long kernel, out long user);'
}
function Busy { $i = 0L; $k = 0L; $u = 0L; [void][TmopMeasure.SysTimes]::GetSystemTimes([ref]$i, [ref]$k, [ref]$u); , @($i, ($k + $u)) }
$samples = @()
$ended = 'still running'
try {
    Start-Sleep -Seconds $WarmUp
    $counters = @(
        "\GPU Engine(pid_$($id)_*)\Utilization Percentage",
        "\GPU Process Memory(pid_$($id)_*)\Dedicated Usage",
        "\GPU Process Memory(pid_$($id)_*)\Shared Usage",
        '\GPU Engine(*engtype_3D)\Utilization Percentage'
    )
    # The process object from Start-Process, never a lookup by PID number: once TMOP has exited
    # its PID may belong to an unrelated process.
    $p = $proc
    if ($p.HasExited) { throw "TMOP pid $id exited during the warm-up" }
    $p.Refresh()
    $cpu0 = $p.TotalProcessorTime; $t0 = [DateTime]::UtcNow; $sys0 = Busy
    for ($i = 0; $i -lt $Seconds; $i++) {
        $set = Get-Counter -Counter $counters -SampleInterval 1 -MaxSamples 1 -ErrorAction SilentlyContinue
        if ($p.HasExited) { $ended = 'exited by itself during the run'; break }
        $p.Refresh()
        $cpu1 = $p.TotalProcessorTime; $t1 = [DateTime]::UtcNow; $sys1 = Busy
        # Kernel time includes idle time; busy = (kernel + user - idle) / (kernel + user).
        $total = $sys1[1] - $sys0[1]
        $mach = if ($total -gt 0) { ($total - ($sys1[0] - $sys0[0])) / $total * 100 } else { 0 }
        $s = [ordered]@{ cpu = ($cpu1 - $cpu0).TotalMilliseconds / ($t1 - $t0).TotalMilliseconds * 100; ws = $p.WorkingSet64; priv = $p.PrivateMemorySize64; handles = $p.HandleCount; threads = $p.Threads.Count
            gpu = 0.0; gpu3d = 0.0; gpu_ded = 0.0; gpu_sh = 0.0; mach_cpu = $mach; mach_3d = 0.0 }
        $cpu0 = $cpu1; $t0 = $t1; $sys0 = $sys1
        $by_type = @{}
        foreach ($c in @($set.CounterSamples)) {
            $path = $c.Path.ToLowerInvariant()
            if ($path -like "*gpu engine(pid_$($id)_*") {
                $type = ($path -replace '.*engtype_([^)]*)\).*', '$1')
                $by_type[$type] = [double]$by_type[$type] + $c.CookedValue
            } elseif ($path -like '*gpu process memory*dedicated usage') { $s.gpu_ded += $c.CookedValue }
            elseif ($path -like '*gpu process memory*shared usage') { $s.gpu_sh += $c.CookedValue }
            elseif ($path -like '*engtype_3d)*') { $s.mach_3d += $c.CookedValue }
        }
        if ($by_type.Count) { $s.gpu = ($by_type.Values | Measure-Object -Maximum).Maximum }
        $s.gpu3d = [double]$by_type['3d']
        $samples += [pscustomobject]$s
    }
} finally {
    # Ends the process this script started, through its own handle (never by PID number).
    if (-not $proc.HasExited) {
        $proc.Kill()
        $proc.WaitForExit(5000) | Out-Null
        $ended = 'stopped by the script (Process.Kill on its own handle)'
    } elseif ($ended -eq 'still running') { $ended = 'exited by itself' }
    Write-Host "TMOP pid $id $ended"
}

function Stat($values) {
    $v = @($values | Where-Object { $_ -ne $null })
    if (-not $v.Count) { return [pscustomobject]@{ avg = [double]::NaN; min = [double]::NaN; max = [double]::NaN } }
    $m = $v | Measure-Object -Average -Minimum -Maximum
    [pscustomobject]@{ avg = $m.Average; min = $m.Minimum; max = $m.Maximum }
}
function Field($line, $key) { if ($line -match "(^| )$key=([^ ]+)") { $matches[2] } }

# TMOP's own lines (see Profile in src/ui/mod.rs), after the warm-up.
$lines = @(if (Test-Path $profile_file) { Get-Content $profile_file | Where-Object { [double](Field $_ 't') -ge $WarmUp } })
$num = { param($key) Stat ($lines | ForEach-Object { $x = Field $_ $key; if ($x -and $x -ne 'n/a') { [double]$x } }) }
$part = { param($key) Stat ($lines | ForEach-Object { $x = Field $_ $key; if ($x -match '^(\d+)x([\d.]+)$' -and [int]$matches[1] -gt 0) { [double]$matches[2] } }) }

$mb = 1MB
$cpu = Stat $samples.cpu
$r = [pscustomobject][ordered]@{
    page            = if ($Popout) { 'popout' } elseif ($Device) { "$Page/$Device" } else { $Page }
    window          = $Window
    label           = $Label
    pid            = $id
    seconds         = $samples.Count
    cpu_core_pct    = [math]::Round($cpu.avg, 1)
    cpu_core_min    = [math]::Round($cpu.min, 1)
    cpu_core_max    = [math]::Round($cpu.max, 1)
    cpu_machine_pct = [math]::Round($cpu.avg / $cores, 2)
    gpu_pct         = [math]::Round((Stat $samples.gpu).avg, 1)
    gpu_3d_pct      = [math]::Round((Stat $samples.gpu3d).avg, 1)
    gpu_mem_mb      = [math]::Round(((Stat $samples.gpu_ded).avg + (Stat $samples.gpu_sh).avg) / $mb, 0)
    ws_mb           = [math]::Round((Stat $samples.ws).avg / $mb, 0)
    private_mb      = [math]::Round((Stat $samples.priv).avg / $mb, 0)
    private_max_mb  = [math]::Round((Stat $samples.priv).max / $mb, 0)
    handles         = [math]::Round((Stat $samples.handles).avg, 0)
    threads         = [math]::Round((Stat $samples.threads).avg, 0)
    fps             = [math]::Round((& $num 'fps').avg, 1)
    frame_ms        = [math]::Round((& $num 'frame_ms').avg, 2)
    ui_ms           = [math]::Round((& $num 'ui_ms').avg, 2)
    ui_thread_pct   = [math]::Round((& $num 'ui_cpu').avg, 1)
    sampler_pct     = [math]::Round((& $num 'sampler_cpu').avg, 1)
    collect_ms      = [math]::Round((& $part 'collect').avg, 1)
    pdh_ms          = [math]::Round((& $part 'pdh').avg, 1)
    gpu_fold_ms     = [math]::Round((& $part 'gpu').avg, 2)
    procs_ms        = [math]::Round((& $part 'procs').avg, 1)
    profile_lines   = $lines.Count
    machine_cpu_pct = [math]::Round((Stat $samples.mach_cpu).avg, 1)
    machine_3d_pct  = [math]::Round((Stat $samples.mach_3d).avg, 1)
    ended           = $ended
}
if ($Csv) { $r | Export-Csv -Path $Csv -Append -NoTypeInformation }
if ($Keep) { Write-Host "kept $home_dir" } else {
    # A folder that cannot be removed (a file still open) must not cost the result.
    try { Remove-Item -Recurse -Force $home_dir -ErrorAction Stop } catch { Write-Warning "could not remove ${home_dir}: $($_.Exception.Message)" }
}
$r
