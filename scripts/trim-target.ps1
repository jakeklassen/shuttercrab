# Empties target/ once it has grown past TARGET_BUDGET_GIB (set in
# mise.toml): Cargo never removes a build's old outputs, and each version
# bump or lockfile change leaves a whole generation of them behind. The
# Windows twin of trim-target.sh.
#
#   mise run trim
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

if (-not (Test-Path target)) { exit 0 }
$budget = [int]$env:TARGET_BUDGET_GIB
$bytes = (Get-ChildItem target -Recurse -File -Force | Measure-Object Length -Sum).Sum
if ($bytes -gt $budget * 1GB) {
  'target/ is {0:N0} GiB, past {1} GiB: cleaning it' -f ($bytes / 1GB), $budget
  cargo clean
  exit $LASTEXITCODE
}
