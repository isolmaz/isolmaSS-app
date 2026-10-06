# Windows distribution

The public source repository [`isolmaz/isolmaSS-app`](https://github.com/isolmaz/isolmaSS-app) builds the program, supplies the Cloudflare self-host template and hosts the signed releases. Versions up to 0.6.0 checked a separate release repository that is now private and archived; they report HTTP 404 when checking for updates and must install 0.6.1 or later from this repository once by hand. From 0.6.1 on, updates come only from here. A release tag is `vMAJOR.MINOR.PATCH` and must match `Cargo.toml`, the Rust PE resource and the NSIS installer version. Never commit secrets or the private signing key; GitHub's automatic source archives are the only source downloads.

## Build and sign on the publisher workstation

Windows x64, Visual Studio C++ Build Tools/Windows SDK, Rust 1.98.0, NSIS and the non-exportable **isolmaSS Update Integrity** (`CN=isolmaSS Update Integrity`) RSA-3072 certificate in `Cert:\CurrentUser\My` are required. The signing script compares its public key with `resources/update-public-key.blob` before signing; the private key never leaves Windows certificate storage. Protect this account and machine: losing the key requires a manually installed trust-root replacement for existing users.

```powershell
$env:ISOLMASS_SIGNING_THUMBPRINT = '<thumbprint of the certificate>'
$env:ISOLMASS_BUILD_DIR = 'target\release-candidate' # Avoid a running target\release\isolmass.exe.
cmd /c release.bat
& "$env:ISOLMASS_BUILD_DIR\release\isolmass.exe" --verify-update "$env:ISOLMASS_BUILD_DIR\release\isolmass-setup.exe"
```

`release.bat` requires the signer and calls `package.bat`. Packaging builds a locked release, checks the executable (≤2.5 MiB) and installer (≤3 MiB) budgets, checks PE file versions, compiles NSIS, writes `isolmass-setup.exe.sha256`, creates the detached `isolmass-setup.exe.sig` and verifies that signature with the **shipped executable's pinned key**. A package without the publisher key is not eligible for auto-update; it will not overwrite an already signed installer in that output directory.

For a manual portable download, archive the same release executable as `isolmass-portable-windows-x64.zip` with `LICENSE` and `THIRD_PARTY_NOTICES.md`. Portable mode does not modify an installed copy or auto-update it. The ZIP is a manual artifact, not the installer that the in-app updater trusts.

## Public release contract

Publish exactly one each of `isolmass-setup.exe`, `isolmass-setup.exe.sig` (raw 384-byte detached signature) and `isolmass-portable-windows-x64.zip` to the matching `vMAJOR.MINOR.PATCH` GitHub Release. Record the lowercase SHA-256 of all three assets in the release notes; the release tag identifies the source commit. Keep the GitHub asset digest available; the app refuses a missing/invalid digest, duplicate asset, draft/prerelease, unexpected release URL, mismatched version or invalid signature. Release builds are immutable once published; a correction uses a **new version** rather than replacing a signed artifact under an existing tag.

The signature covers the UTF-8 bytes `isolmaSS-update-v1\n<VERSION>\n<INSTALLER-SHA256-LOWERCASE-HEX>\n`. `scripts/sign-update.ps1` implements the version-general form. HTTPS delivery and SHA-256 establish transport/integrity; only the pinned publisher signature establishes update authenticity. The app re-verifies the staged file immediately before launch.

The installer is not Authenticode-signed; Windows SmartScreen may display a reputation warning. Never automate bypassing or acceptance of that warning. Installation asks for consent in the app before download and launch; `--check-update` only checks availability.

## Installation and rollback

The NSIS installer installs per user at `%LOCALAPPDATA%\isolmaSS` without elevation. `isolmass.exe` runs as a background tray app; installer startup is not tied to double-clicking the EXE. Updates pass `/S /UPDATE /WAITPID=<pid>` after editing/settings and synchronous clipboard/save work finishes. The installer waits for the old process, stages the new executable, keeps `isolmass.previous.exe`, runs `--health-check` on the installed candidate, starts the tray and checks that the window is present. If activation fails, it restores the executable and display version, displays an error and retains the backup when restoration is prevented. Never delete a remaining `.previous.exe` without diagnosing the failed transaction.

The rollback covers application activation, **not** user screenshots or settings. Users should retain backups of personal data independently. Successful updates remove the rollback executable. Uninstall offers to keep personal configuration and screenshots.

## Cloudflare deployment

The source release tag contains `cloudflare/` (a Worker with SQLite Durable Object storage). The Windows app explicitly requests `workers-scripts.write` and `memberships.read` from the publisher's verified Public OAuth client. Before publication, ensure both are configured as required scopes on that client and that the client is **public**: a private client can only be authorized by members of its own account, and other users then fail (for example with `HTTP 403` on the Workers subdomain check); the code is exchanged on the user's computer and installation targets only the account the user authorizes and selects. GitHub login, R2/D1 activation, secret pasting and a custom domain are unnecessary. `release.bat` and the updater do **not** touch Cloudflare resources; each account owner explicitly authorizes installation. See the [Worker API](docs/CLOUDFLARE.md#worker-api). `ss.isolmaz.com` is a static site maintained outside this repository and hosts no screenshots.

Local Worker tests, Rust tests and a signed package are **not** proof that Cloudflare accepts a real installation in another account. Before a release that changes setup or the Worker, install and upload with a real account; if that finds a defect, publish a corrected new version instead of replacing signed assets.

## Verification boundaries

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1
```

The script runs `cargo fmt --check`, Clippy with `-D warnings`, the tests and `node --check cloudflare/worker.mjs`. There is no hosted CI: GitHub Actions is disabled for the source repository and every release is verified on the publisher's machine.

The full interactive `--smoke-test` requires a disposable desktop because it captures the active screen and replaces the clipboard; it compiles a temporary installer and removes it afterward. `--verify-update PATH` is safe for a local installer and matching `.exe.sig` sidecar: it checks file version and the pinned signature without executing the installer. Test updater rollback with an isolated Windows profile or VM rather than overwriting a currently running user installation. The release and its source tag share one version in this repository; the release notes record the artifact hashes.
