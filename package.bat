@echo off
setlocal enabledelayedexpansion

set "BUILD_DIR=target"
if defined ISOLMASS_BUILD_DIR set "BUILD_DIR=%ISOLMASS_BUILD_DIR%"
set "BIN_PATH=%BUILD_DIR%\release\isolmass.exe"
set "NSI_PATH=installer.nsi"
set "SETUP_PATH=%BUILD_DIR%\release\isolmass-setup.exe"
set "DIGEST_PATH=%BUILD_DIR%\release\isolmass-setup.exe.sha256"
set MAX_BUDGET=3145728
set MAX_BINARY_BUDGET=2621440
if not defined ISOLMASS_SIGNING_THUMBPRINT (
    if /I "%REQUIRE_SIGNING%"=="1" (
        echo [ERROR] A signed release requires ISOLMASS_SIGNING_THUMBPRINT.
        exit /b 1
    )
    if exist "%SETUP_PATH%.sig" (
        echo [ERROR] Refusing to overwrite a signed artifact without a publisher key. Use another build directory or sign this package.
        exit /b 1
    )
)

set PRODUCT_VERSION=
for /f "usebackq delims=" %%V in (`powershell -NoProfile -Command "$ErrorActionPreference='Stop'; $json = cargo metadata --locked --no-deps --format-version 1; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; $metadata = $json | ConvertFrom-Json; $version = ($metadata.packages | Where-Object name -eq 'isolmass').version; if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid package version' }; $version"`) do set PRODUCT_VERSION=%%V
if not defined PRODUCT_VERSION (
    echo [ERROR] Could not read the package version from Cargo metadata
    exit /b 1
)

echo ============================================================
echo  isolmaSS %PRODUCT_VERSION% Distribution Packaging
echo ============================================================

echo [INFO] Building production binary...
cargo build --release --locked --target-dir "%BUILD_DIR%"
if errorlevel 1 exit /b 1


for %%I in ("%BIN_PATH%") do set BIN_SIZE=%%~zI
if !BIN_SIZE! gtr %MAX_BINARY_BUDGET% (
    echo [ERROR] Executable size !BIN_SIZE! exceeds %MAX_BINARY_BUDGET% bytes
    exit /b 1
)

set "MAKENSIS_CMD="
if defined MAKENSIS set "MAKENSIS_CMD=%MAKENSIS%"
if not defined MAKENSIS_CMD (
    where makensis >nul 2>nul
    if not errorlevel 1 set "MAKENSIS_CMD=makensis"
)
if not defined MAKENSIS_CMD (
    for /f "usebackq delims=" %%P in (`powershell -NoProfile -Command "$ErrorActionPreference='SilentlyContinue'; $root = Join-Path $env:LOCALAPPDATA 'Programs'; $dirs = Get-ChildItem -LiteralPath $root -Directory -Filter 'nsis-*'; $best = $dirs | Sort-Object { try { [version]($_.Name -replace '^nsis-','') } catch { [version]'0.0' } } | Select-Object -Last 1; if ($null -ne $best) { $exe = Join-Path $best.FullName 'makensis.exe'; if (Test-Path -LiteralPath $exe) { $exe } }"`) do set "MAKENSIS_CMD=%%P"
)
if not defined MAKENSIS_CMD (
    echo [ERROR] makensis was not found. Set MAKENSIS to the full path of makensis.exe, put makensis on PATH, or install NSIS so that %%LOCALAPPDATA%%\Programs\nsis-*\makensis.exe exists.
    exit /b 1
)

echo [INFO] Compiling NSIS installer...
"!MAKENSIS_CMD!" /DPRODUCT_VERSION=%PRODUCT_VERSION% "/DPRODUCT_EXE_SOURCE=%BIN_PATH%" "/DPRODUCT_OUTPUT=%SETUP_PATH%" "%NSI_PATH%"
if errorlevel 1 exit /b 1
if not exist "%SETUP_PATH%" (
    echo [ERROR] Installer output does not exist: %SETUP_PATH%
    exit /b 1
)


powershell -NoProfile -Command "$ErrorActionPreference='Stop'; $expected = [version]'%PRODUCT_VERSION%'; foreach ($path in @('%BIN_PATH%','%SETUP_PATH%')) { $info = (Get-Item -LiteralPath $path).VersionInfo; foreach ($field in @('ProductVersion','FileVersion')) { $raw = $info.$field; $parsed = $null; if (-not [version]::TryParse($raw, [ref]$parsed)) { throw ('Unparseable ' + $field + ' on ' + $path + ': ' + $raw) }; if ($parsed.Major -ne $expected.Major -or $parsed.Minor -ne $expected.Minor -or $parsed.Build -ne $expected.Build) { throw ('Artifact ' + $field + ' mismatch on ' + $path + ': ' + $raw + ' != ' + '%PRODUCT_VERSION%') } } }"
if errorlevel 1 exit /b 1

for %%I in ("%SETUP_PATH%") do set SETUP_SIZE=%%~zI
if !SETUP_SIZE! gtr %MAX_BUDGET% (
    echo [ERROR] Installer size !SETUP_SIZE! exceeds %MAX_BUDGET% bytes
    exit /b 1
)

powershell -NoProfile -Command "$ErrorActionPreference='Stop'; $stream = [IO.File]::OpenRead('%SETUP_PATH%'); $sha = [Security.Cryptography.SHA256]::Create(); try { $hash = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-','').ToLowerInvariant() } finally { $sha.Dispose(); $stream.Dispose() }; if ($hash -notmatch '^[0-9a-f]{64}$') { throw 'Invalid installer checksum' }; [IO.File]::WriteAllText('%DIGEST_PATH%', $hash + '  isolmass-setup.exe' + [Environment]::NewLine, [Text.Encoding]::ASCII)"
if errorlevel 1 exit /b 1

if defined ISOLMASS_SIGNING_THUMBPRINT (
    powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\sign-update.ps1" -Installer "%SETUP_PATH%" -Version "%PRODUCT_VERSION%" -Thumbprint "%ISOLMASS_SIGNING_THUMBPRINT%"
    if errorlevel 1 exit /b 1
    "%BIN_PATH%" --verify-update "%SETUP_PATH%"
    if errorlevel 1 exit /b 1
) else (
    echo [WARN] No detached signature was produced; this package cannot be published for automatic updates.
)
echo [SUCCESS] Installer: %SETUP_PATH%
echo [INFO] Version: %PRODUCT_VERSION%
echo [INFO] Size: !SETUP_SIZE! bytes
for /f "usebackq tokens=1" %%H in ("%DIGEST_PATH%") do echo [INFO] SHA-256: %%H
exit /b 0
