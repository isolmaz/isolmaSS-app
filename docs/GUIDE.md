# isolmaSS guide

The interface is in English and Turkish. It follows the Windows display language (Turkish for Turkish Windows, English otherwise); change it under **Settings → General → Language**. Labels below are the English ones.

Everything about using the app. Sharing and the Worker are covered in [CLOUDFLARE.md](CLOUDFLARE.md); security in [SECURITY.md](../SECURITY.md).

## Use

1. Start `isolmass.exe`; it stays in the notification area. Launching it again opens Settings of the running instance.
2. Press **PrintScreen**, click the tray icon, or choose **Take screenshot** from the tray menu. The shortcut and an optional delay are configurable.
3. Drag a region of at least 8 × 8 pixels, or click a visible window when window snapping is on. Resize from the corners or edge midpoints; drag the size label above the top-left corner to move the selection.
4. Draw with **Select/move**, **Frame**, **Arrow** and **Pen**, or open the chevron for **Highlighter**, **Text**, **Numbered steps**, **Blur** and opaque **Redact**. F10/Apps lists every command. Select a drawing to move, recolor, resize, change its width or delete it. Undo and redo cover every edit.
5. Choose **Copy**, **Save**, Save as (Ctrl+Shift+S) or **Upload**. Editor chrome, handles and unfinished previews never appear in the output.

**Upload** closes the editor at once and shows a small card in the bottom-right corner: *Uploading…*, then *Uploaded · link copied* with the link, **Copy** and **Open**. The link is already on the clipboard. If the transfer fails the card shows the reason and **Try again**; the clipboard is left unchanged. The first **Upload** opens Cloudflare setup instead (see below).

**Redact** replaces covered pixels opaquely and is drawn after other annotations. Blur and highlighter are visual effects, **not** secure redaction. Inspect the image before sharing sensitive material.

### Keyboard

| Shortcut | Action |
|---|---|
| Capture shortcut (default PrintScreen) | Start a capture after the configured delay |
| V / R / A / P / T / H / N / B / M | Select, frame, arrow, pen, text, highlighter, numbered step, blur, opaque redaction |
| Ctrl+C or Enter | Copy the selection |
| Ctrl+S / Ctrl+Shift+S | Save / Save as |
| Ctrl+U | Upload and copy the link (first use: Cloudflare setup) |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+, | Settings |
| F10 or Apps | Toolbar command menu |
| Delete or Backspace | Delete the selected annotation |
| Arrow keys / Shift+arrows | Move the selection or object by 1 / 10 pixels |
| Ctrl+arrows | Resize the selection when no object is selected |
| Shift while drawing | Square rectangle/redaction; 45° arrow angles |
| Esc or right-click | Cancel the current edit, deselect, clear the selection, or close the editor |

While editing text, Enter commits; Ctrl+C, Ctrl+S and Ctrl+U commit before exporting. Text is single-line, up to 16 KiB; pasted newlines become spaces. The toolbar is icon-only; hovering shows each tool's full name. The color square shows the active color and opens the Windows color picker. The compact line-width button opens a wheel underneath for **1–64 px** (click a row, drag or scroll it, use the arrow keys, or type a number and press Enter). The active and custom colors persist.

## Settings

Settings is a compact Windows 11 dialog with tabs on top and its own taskbar button, like the Cloudflare window:

- **General** — capture shortcut, delay, theme (system, light, dark), language (system, English, Türkçe), start with Windows, save notification.
- **Saving** — folder, PNG or JPEG, JPEG quality.
- **Editor** — a 20-color palette plus a custom color, line width 1–64 px, window snapping, close after an action.
- **Sharing** — Cloudflare connection status and the Cloudflare window.
- **Updates** — automatic checks and a manual check with inline progress.

Record a shortcut with Ctrl/Alt/Shift/Win plus a letter, digit, F1–F24 or PrintScreen; bare PrintScreen also works. A combination already taken by Windows or another app is reported and the current shortcut is kept. If the shortcut is unavailable at startup, the app offers Ctrl+Shift+S for that session. Controls, scroll bars and menus follow the light or dark theme.

A language change applies after **Save**. The tray menu offers **Take screenshot**, **Settings**, **Screenshots folder**, **Check for updates**, recent captures and **Quit isolmaSS**.

## Updates

Update checks read the public releases of this repository, [`isolmaz/isolmaSS-app`](https://github.com/isolmaz/isolmaSS-app/releases). Versions up to 0.6.0 checked a release repository that is no longer public and report *HTTP 404*; install the latest version from the [download page](https://ss.isolmaz.com/download) once and updates work again. A newer release offers **Install / Later / Skip this version**; nothing installs without that choice, and a skipped version is offered again only by a manual check. Installation waits for any open editor or Settings window, verifies the installer's publisher signature again, closes the app, installs and restarts it. The installer keeps a rollback copy until the new version passes a startup check. The signature is application-level: Windows SmartScreen may still warn, and the app never bypasses that warning. Portable ZIPs are for manual use and do not update an installed copy. See [SECURITY.md](../SECURITY.md) and [DISTRIBUTION.md](../DISTRIBUTION.md).

## Files and privacy

- Screenshots: the Windows Pictures folder → `Screenshots` by default.
- Settings: `%APPDATA%\isolmaSS\settings.json` (at most 64 KiB, validated, saved atomically under a cross-process lock). It holds the Worker address but **no key or token**.
- Worker keys and the optional image password: `%LOCALAPPDATA%\isolmaSS\cloud-credentials.bin`, encrypted for the current Windows user with DPAPI.
- Upload staging: an encoded copy of the selection in the user's temp folder, deleted when the upload card closes.
- Diagnostics: `%LOCALAPPDATA%\isolmaSS\logs\diagnostic.log` (rotated near 1 MiB). No pixels, annotation text or keystrokes; errors may include local paths.
- Update downloads: `%LOCALAPPDATA%\isolmaSS\updates`.

Invalid settings JSON is copied to `settings.corrupt.json` before defaults are used. Exported PNG/JPEG and clipboard images are opaque. Save as replaces a file only after encoding and flushing succeed.

## Build and command line

Windows x64, the Windows SDK / Visual Studio C++ Build Tools, Rust **1.98.0** and NSIS are required. `Cargo.lock` pins dependencies.

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1   # fmt, clippy, tests, Worker syntax
cmd /c package.bat
```

`package.bat` verifies versions, size budgets and SHA-256. Set `ISOLMASS_BUILD_DIR=target\release-candidate` while `target\release\isolmass.exe` is running. Verification runs locally with the commands above; there is no hosted CI and GitHub Actions is disabled. `release.bat` needs the publisher's non-exportable signing key. Release steps: [DISTRIBUTION.md](../DISTRIBUTION.md).

Command line: `--capture-once` (editor without the tray; waits for an upload card to close), `--settings`, `--check-update`, `--verify-update PATH`, `--benchmark N`, `--fix-printscreen` and `--help`. `--smoke-test` needs an interactive desktop and **changes the clipboard**.

Automated tests do not cover the interactive UI. Before a release, check by hand: both themes and both languages, DPI scaling, every tool and export path, tray and Settings flows, updater rollback in an isolated Windows profile, and Cloudflare setup and upload with a real account. MIT license: [LICENSE](../LICENSE); dependencies: [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

