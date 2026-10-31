# 稳定性长测：启动 wp-host（默认关闭暂停判定），每 30 s 采样 CPU / GPU / 句柄。
#
# 用法（pwsh 7）：
#   pwsh -NoProfile -File scripts/soak-sampler.ps1 -Duration 1800
#
# 产物：
#   out/soak-30m.csv        采样表（elapsed_s / cpu_pct / ws_mb / gpu_pct / threads / handles）
#   out/soak-30m.trace.log  wp-host 日志（tracing 默认写 stdout）
#   out/soak-30m.stderr.log 预留的 stderr
param(
    [int]$Duration = 1800,
    [int]$IntervalSeconds = 30,
    [string]$Tag = 'soak-30m'
)

$ErrorActionPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $root 'out'
New-Item -ItemType Directory -Force $outDir | Out-Null

$exe = Join-Path $root 'target\debug\wp-host.exe'
$trace = Join-Path $outDir "$Tag.trace.log"
$stderr = Join-Path $outDir "$Tag.stderr.log"
$csv = Join-Path $outDir "$Tag.csv"

if (-not (Test-Path $exe)) { throw "找不到 $exe，先 cargo build -p wp-host" }
Remove-Item $trace, $stderr, $csv -ErrorAction SilentlyContinue

$env:RUST_LOG = 'info'
$p = Start-Process -FilePath $exe `
    -ArgumentList '--no-pause', '--duration', "$Duration" `
    -RedirectStandardOutput $trace -RedirectStandardError $stderr `
    -PassThru -WindowStyle Hidden

"time,elapsed_s,alive,cpu_pct,ws_mb,gpu_pct,threads,handles" | Set-Content -Encoding utf8 $csv
$start = Get-Date
$prevCpu = $p.TotalProcessorTime
$cores = [Environment]::ProcessorCount

while ($true) {
    Start-Sleep -Seconds $IntervalSeconds
    $p.Refresh()
    if ($p.HasExited) { break }

    $nowCpu = $p.TotalProcessorTime
    $cpuPct = [math]::Round((($nowCpu - $prevCpu).TotalSeconds / $IntervalSeconds / $cores) * 100, 1)
    $prevCpu = $nowCpu
    $elapsed = [math]::Round(((Get-Date) - $start).TotalSeconds, 0)

    # GPU Engine 计数器按 pid 过滤；老驱动上可能没有这个计数器，取不到就留空。
    $gpuPct = ''
    $samples = Get-Counter '\GPU Engine(*)\Utilization Percentage' -MaxSamples 1
    if ($samples) {
        $sum = ($samples.CounterSamples |
            Where-Object { $_.InstanceName -like "*pid_$($p.Id)_*" } |
            Measure-Object CookedValue -Sum).Sum
        if ($null -ne $sum) { $gpuPct = [math]::Round($sum, 1) }
    }

    $ws = [math]::Round($p.WorkingSet64 / 1MB, 1)
    "$(Get-Date -Format o),$elapsed,1,$cpuPct,$ws,$gpuPct,$($p.Threads.Count),$($p.HandleCount)" |
        Add-Content -Encoding utf8 $csv
}

$p.WaitForExit()
"exit_code=$($p.ExitCode)" | Add-Content -Encoding utf8 $csv
exit $p.ExitCode
