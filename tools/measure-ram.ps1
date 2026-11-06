# Prints Glint's memory use, the same "Memory (private working set)" number
# Task Manager shows, plus the full working set and private bytes.
#
# Usage:  powershell -ExecutionPolicy Bypass -File tools\measure-ram.ps1

param([string]$Name = "glint")

$perf = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "Name='$Name'"
if (-not $perf) {
    Write-Host "$Name is not running."
    exit 1
}
$proc = Get-Process -Name $Name | Select-Object -First 1

"{0,-28} {1,8:N1} MB" -f "Private working set:", ($perf.WorkingSetPrivate / 1MB)
"{0,-28} {1,8:N1} MB" -f "Working set (incl. shared):", ($proc.WorkingSet64 / 1MB)
"{0,-28} {1,8:N1} MB" -f "Private bytes (commit):", ($proc.PrivateMemorySize64 / 1MB)
"{0,-28} {1,8}" -f "Threads:", $proc.Threads.Count
"{0,-28} {1,8}" -f "Handles:", $proc.HandleCount
