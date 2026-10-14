# Chooses which cargo test filters CI runs for a change. Used by the `plan` job in .github/workflows/ci.yml.
# usage in CI:  pwsh scripts/ci-test-plan.ps1 -Base <base sha> -Head <head sha>
# manual test:  pwsh scripts/ci-test-plan.ps1 -Files src/overlay/render.rs,cloudflare/worker.mjs
# Prints the plan and, when GITHUB_OUTPUT is set, writes the outputs `filters` (JSON array),
# `has_tests` (true/false) and `worker` (true/false).
param(
    [string]$Base = '',
    [string]$Head = 'HEAD',
    [string[]]$Files
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# Module list: stems of src/*.rs except main.
$modules = @(Get-ChildItem -Path (Join-Path $root 'src') -Filter '*.rs' -File |
        ForEach-Object { $_.BaseName } |
        Where-Object { $_ -ne 'main' } |
        Sort-Object)

# A change to one of these can affect every module.
$globalPatterns = @(
    '^Cargo\.toml$', '^Cargo\.lock$', '^build\.rs$', '^rust-toolchain\.toml$',
    '^resources/', '^src/main\.rs$', '^src/theme\.rs$', '^src/drawing\.rs$',
    '^\.github/workflows/', '^scripts/ci-test-plan\.ps1$'
)

$everything = $false
$changed = @()
if ($PSBoundParameters.ContainsKey('Files')) {
    $changed = @($Files | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
} elseif ([string]::IsNullOrWhiteSpace($Base) -or $Base -match '^0+$') {
    $everything = $true
} else {
    $changed = @(git -C $root diff --name-only "$Base...$Head")
    if ($LASTEXITCODE -ne 0) {
        Write-Host "git diff $Base...$Head failed; testing everything."
        $everything = $true
        $changed = @()
    }
}
$changed = @($changed | ForEach-Object { $_.Trim().Replace('\', '/') } | Where-Object { $_ })

$worker = $everything
$sourceChanged = $false
$selected = [System.Collections.Generic.HashSet[string]]::new()
foreach ($file in $changed) {
    foreach ($pattern in $globalPatterns) {
        if ($file -match $pattern) { $everything = $true }
    }
    if ($file -like 'cloudflare/*') { $worker = $true }
    if ($file -match '^src/([^/]+?)(\.rs)?(/.*)?$') {
        $name = $Matches[1]
        if ($file -like '*.rs') { $sourceChanged = $true }
        if ($modules -contains $name) { [void]$selected.Add($name) }
    }
}

$filters = @()
if ($everything) {
    $worker = $true
    $filters = @($modules | ForEach-Object { "$_::" })
} else {
    $filters = @($modules | Where-Object { $selected.Contains($_) } | ForEach-Object { "$_::" })
    # The translation coverage test scans every source file.
    if ($sourceChanged -and $filters -notcontains 'i18n::') { $filters += 'i18n::' }
}

$json = '[' + (($filters | ForEach-Object { '"' + $_ + '"' }) -join ',') + ']'
$hasTests = if ($filters.Count -gt 0) { 'true' } else { 'false' }
$workerOut = if ($worker) { 'true' } else { 'false' }

Write-Host "changed files: $($changed.Count)$(if ($everything) { ' (everything)' })"
Write-Host "filters=$json"
Write-Host "has_tests=$hasTests"
Write-Host "worker=$workerOut"
if ($env:GITHUB_OUTPUT) {
    Add-Content -Path $env:GITHUB_OUTPUT -Value "filters=$json", "has_tests=$hasTests", "worker=$workerOut"
}
