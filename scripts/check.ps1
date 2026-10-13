# Local checks for every change; run this before each commit. Tests are not run on this machine:
# .github/workflows/ci.yml runs them on GitHub for every pull request.
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
if (Get-Command node -ErrorAction SilentlyContinue) {
    Step 'worker syntax' { node --check cloudflare/worker.mjs }
} else {
    Write-Host 'SKIPPED: worker syntax (Node.js not found)' -ForegroundColor Yellow
}
Write-Host 'Local checks passed. Tests run in CI.' -ForegroundColor Green
Write-Host 'To re-run one failing test: cargo test --locked <test_name> -- --test-threads=1'
