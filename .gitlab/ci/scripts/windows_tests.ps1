# test:windows (.gitlab-ci.yml): the Windows test run of GitHub CI's
# "Build and test (Windows)" job, on a wintest/<name> branch, before main.
#
# The commands are read from this commit's own .github/workflows/ci.yml, never
# restated here, so the two runs cannot drift: every top-level `env:` entry,
# then the build-and-test-windows job's `cargo test ... --no-run` compile step
# and its `cargo test` step. Exit code = cargo's.

# 'Continue', not 'Stop': a native command's stderr line is output, not an
# error. Every native call is checked through $LASTEXITCODE.
$ErrorActionPreference = 'Continue'

$ci = Get-Content .github/workflows/ci.yml

$e = [array]::IndexOf($ci, 'env:')
if ($e -ge 0) {
  for ($k = $e + 1; $k -lt $ci.Count -and $ci[$k] -match '^  ([A-Z0-9_]+):\s*(\S.*)$'; $k++) {
    $value = $Matches[2].Trim().Trim('"', "'")
    Set-Item -Path "env:$($Matches[1])" -Value $value
    Write-Output "env $($Matches[1])=$value"
  }
}

$s = [array]::IndexOf($ci, '  build-and-test-windows:')
if ($s -lt 0) { Write-Output 'no build-and-test-windows job in .github/workflows/ci.yml'; exit 2 }
$end = $ci.Count
for ($k = $s + 1; $k -lt $ci.Count; $k++) { if ($ci[$k] -match '^  [A-Za-z0-9_-]+:\s*$') { $end = $k; break } }
$runs = @($ci[$s..($end - 1)] | Where-Object { $_ -match '^\s+run: (cargo test .*)$' } | ForEach-Object { ($_ -replace '^\s+run: ', '').Trim() })
$compile = $runs | Where-Object { $_ -match '--no-run' } | Select-Object -First 1
$test = $runs | Where-Object { $_ -notmatch '--no-run' } | Select-Object -First 1
if (-not $compile -or -not $test) { Write-Output 'could not read the two cargo test steps of build-and-test-windows'; exit 2 }

& rustup show active-toolchain
foreach ($cmd in $compile, $test) {
  Write-Output "`$ $cmd"
  # cmd.exe runs the line as written: PowerShell can eat a bare `--`.
  & cmd.exe /d /c $cmd
  if ($LASTEXITCODE) { exit $LASTEXITCODE }
}
