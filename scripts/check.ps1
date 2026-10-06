# Local checks for every change. The project has no hosted CI; run this before each commit.
# usage: powershell -ExecutionPolicy Bypass -File scripts\check.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

function Step($name, [scriptblock]$command) {
    Write-Host "==> $name"
    & $command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $name" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Step 'cargo fmt' { cargo fmt --check }
Step 'cargo clippy' { cargo clippy --all-targets --locked -- -D warnings }
Step 'cargo test' { cargo test --locked -- --test-threads=1 --skip test_tray_manager_lifecycle }
if (Get-Command node -ErrorAction SilentlyContinue) {
    Step 'worker syntax' { node --check cloudflare/worker.mjs }
} else {
    Write-Host 'SKIPPED: worker syntax (Node.js not found)' -ForegroundColor Yellow
}
Write-Host 'All checks passed.' -ForegroundColor Green
