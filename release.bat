@echo off
setlocal
if not defined ISOLMASS_SIGNING_THUMBPRINT (
    echo [ERROR] Set ISOLMASS_SIGNING_THUMBPRINT to the pinned CurrentUser certificate thumbprint.
    exit /b 1
)
set REQUIRE_SIGNING=1
call package.bat
exit /b %errorlevel%
