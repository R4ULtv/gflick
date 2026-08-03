[CmdletBinding()]
param(
    [ValidateRange(1, 3600)]
    [int]$SampleIntervalSeconds = 1,

    [switch]$Once,

    [switch]$NoClear
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$agentPath = [System.IO.Path]::GetFullPath(
    (Join-Path $env:LOCALAPPDATA 'open-hub\bin\open-hub-agent.exe')
)
$logicalProcessorCount = [Environment]::ProcessorCount

do {
    $agent = Get-Process -Name 'open-hub-agent' -ErrorAction SilentlyContinue |
        Where-Object {
            $_.Path -and
            [System.IO.Path]::GetFullPath($_.Path) -eq $agentPath
        } |
        Select-Object -First 1

    if (-not $NoClear -and -not $Once) {
        Clear-Host
    }

    if (-not $agent) {
        Write-Host "Open Hub agent is not running at $agentPath"
    }
    else {
        $counter = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process `
            -Filter "IDProcess=$($agent.Id)"

        if (-not $counter) {
            Write-Host "Open Hub agent exited while its counters were being sampled."
        }
        else {
            [pscustomobject]@{
                Time                   = Get-Date -Format 'HH:mm:ss'
                PID                    = $agent.Id
                Uptime                 = (Get-Date) - $agent.StartTime
                CPU_Normalized_Percent = [math]::Round(
                    [double]$counter.PercentProcessorTime / $logicalProcessorCount,
                    3
                )
                CPU_Total_Seconds      = [math]::Round($agent.CPU, 3)
                WorkingSet_MiB         = [math]::Round(
                    [double]$counter.WorkingSet / 1MB,
                    2
                )
                PrivateWorkingSet_MiB  = [math]::Round(
                    [double]$counter.WorkingSetPrivate / 1MB,
                    2
                )
                PrivateBytes_MiB       = [math]::Round(
                    [double]$counter.PrivateBytes / 1MB,
                    2
                )
                IO_Read_KiB_s          = [math]::Round(
                    [double]$counter.IOReadBytesPersec / 1KB,
                    2
                )
                IO_Write_KiB_s         = [math]::Round(
                    [double]$counter.IOWriteBytesPersec / 1KB,
                    2
                )
                IO_Other_Ops_s         = $counter.IOOtherOperationsPersec
                PageFaults_s           = $counter.PageFaultsPersec
                Threads                = $counter.ThreadCount
                Handles                = $counter.HandleCount
            } | Format-List
        }
    }

    if (-not $Once) {
        Start-Sleep -Seconds $SampleIntervalSeconds
    }
} while (-not $Once)
