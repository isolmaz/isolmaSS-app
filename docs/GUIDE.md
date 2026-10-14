# isolmaSS guide

The interface is in English and Turkish. It follows the Windows display language (Turkish for Turkish Windows, English otherwise); change it under **Settings → General → Language**. Labels below are the English ones.

Everything about using the app. Sharing and the Worker are covered in [CLOUDFLARE.md](CLOUDFLARE.md); security in [SECURITY.md](../SECURITY.md).

## Use

1. Start `isolmass.exe`; it stays in the notification area. Launching it again opens Settings of the running instance.
2. Press **PrintScreen**, click the tray icon, or choose **Take screenshot** from the tray menu. The shortcut and an optional delay are configurable. **Capture last region** in the tray menu opens the editor with the region you last copied, saved, uploaded or pinned already selected. Copy, Save, Upload and Pin to screen set it; it is forgotten when the app quits, the menu item is grayed out until then, and the editor opens without a region if that area no longer fits the screens.
3. Drag a region of at least 8 × 8 pixels, or click a visible window when window snapping is on. **Ctrl+A** selects the whole monitor under the pointer instead (with a region already selected, it changes that region to the monitor, and Ctrl+Z undoes it). Resize from the corners or edge midpoints; drag the size label above the top-left corner to move the selection.
4. Draw with **Select/move**, **Frame**, **Arrow** and **Pen**, or open the chevron for **Ellipse**, **Line**, **Highlighter**, **Text**, **Numbered steps**, **Blur**, opaque **Redact** and **Pick color**. F10/Apps lists every command. Select a drawing to move, recolor, resize, change its width or delete it. The dashed selection box sits outside the stroke, so it never cuts through a thick line. Undo and redo cover every edit.
5. Choose **Copy**, **Save**, Save as (Ctrl+Shift+S), **Upload** or **Pin to screen** (Ctrl+T). Editor chrome, handles and unfinished previews never appear in the output.

**Upload** closes the editor at once and shows a small card in the bottom-right corner: *Uploading… 42%* with real progress, then *Uploaded · link copied* with the link, **Copy** and **Open**. The link is already on the clipboard. If the transfer fails the card shows the reason (for example a full Worker or the daily limit, with what to change) and **Try again**; the clipboard is left unchanged. The first **Upload** opens Cloudflare setup instead (see below).

Once the card shows *Uploaded*, **drag the image from the card** (anywhere except its buttons) into a chat, a browser upload field or a folder. The drop receives the image as a file and the share link as text, so a text field gets the link. The card stays while you drag. A fading card cannot be dragged. If the file copy cannot be made, the drag still carries the link.

**Pin to screen** (Ctrl+T, or the F10 menu) closes the editor and keeps the selection, with your drawings, on the desktop in a borderless always-on-top window with a thin amber border. Drag it to move it; scroll to zoom around the pointer from 25 % to 400 %; double-click or press Esc to close it; press Ctrl+C to copy the image. Right-click for **Copy**, **Save as…** and **Close**. A pin appears without taking the keyboard focus from the app you were using; click it once, and then Esc and Ctrl+C work. Pins do not appear in the taskbar and are not saved: they close when you close them or quit the app.

**Copy text** (Ctrl+Shift+C, or the F10 menu) reads the text in the selection and puts it on the clipboard, one line per line, and reports *Text copied* in a notice inside the editor; the editor stays open unless **Close after an action** is on, in which case it closes and the same message appears as a tray notification. **Redact emails and phone numbers** (Ctrl+Shift+R) adds an opaque Redact box over each email address and phone number it finds, as a single undo step, and keeps the editor open; the notice says *Redacted N items*. If nothing is found, the notice says *No text found in the selection* or *No email addresses or phone numbers found*. Both run in the background and ignore a second request until the first finishes. Both need a Windows text recognition language: install one under **Settings › Time & language › Language & region**; without one a dialog tells you so. While a text box is open, Ctrl+T, Ctrl+Shift+C and Ctrl+Shift+R commit the text first, so it is included. Recognition runs locally with Windows.Media.Ocr and reads the image as exported, so redactions and blur you already applied are respected. It reads what it can recognize: always check the result, and add a Redact box for anything it missed. Phone numbers are 7–15 digits and may include `+`, spaces, dots, dashes and one parenthesised area code; dates, times such as 09.00-18.00, year ranges such as 2019-2024, prices and version numbers are left alone. Redaction errs on the side of privacy: other long digit sequences that look like phone numbers (order numbers, IBAN or card number groups) are covered too. Press Ctrl+Z to remove all the boxes at once, or select one and press Delete.

**Redact** replaces covered pixels opaquely and is drawn after other annotations. Blur and highlighter are visual effects, **not** secure redaction. Inspect the image before sharing sensitive material.

### Keyboard

| Shortcut | Action |
|---|---|
| Capture shortcut (default PrintScreen) | Start a capture after the configured delay |
| V / R / E / A / L / P / T / H / N / B / M | Select, frame, ellipse, arrow, line, pen, text, highlighter, numbered step, blur, opaque redaction |
| I | Pick color: click a pixel of the capture to copy its `#RRGGBB` code (needs a selected region; Esc goes back to the previous tool) |
| Ctrl+A | Select the whole monitor under the pointer (in a text box: select all its text) |
| Ctrl+C or Enter | Copy the selection |
| Ctrl+S / Ctrl+Shift+S | Save / Save as |
| Ctrl+U | Upload and copy the link (first use: Cloudflare setup) |
| Ctrl+T | Pin the selection to the screen |
| Ctrl+Shift+C | Copy the text in the selection |
| Ctrl+Shift+R | Redact emails and phone numbers in the selection |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+, | Settings |
| F10 or Apps | Toolbar command menu |
| Delete or Backspace | Delete the selected annotation |
| Arrow keys / Shift+arrows | Move the selection or object by 1 / 10 pixels |
| Ctrl+arrows | Resize the selection when no object is selected |
| Shift while drawing | Square rectangle/redaction, circle; 45° arrow and line angles |
| Esc or right-click | Cancel the stroke or drag in progress, then the current edit, deselect, clear the selection, or close the editor |
| Ctrl+Z after clearing | Restore the selection and drawings that Esc or a new region discarded |

While editing text, Enter commits and Shift+Enter starts a new line; Ctrl+C, Ctrl+S and Ctrl+U commit before exporting. Click inside the text to move the caret (Shift-click or drag selects), Ctrl+Left/Right jump by word, Ctrl+Backspace/Delete delete a word and Ctrl+X cuts. Up/Down move between lines (Shift extends the selection) and Home/End go to the start and end of the current line. With the Text tool, clicking existing text edits it. Text can have several lines, up to 16 KiB; pasted line breaks are kept and tabs become four spaces. **Ctrl+B**, or the *Text background* button next to the color and width controls, puts the text on a rounded box filled with the active color; the text turns black or white to stay readable. It works while typing, with the Text tool active and on a selected text (one undo step), and the choice is remembered for new text. The toolbar is icon-only; hovering shows each tool's full name and shortcut. Toolbar buttons act when released, so dragging off a button cancels it. A plain click outside the selection only deselects; dragging there starts a new region. The color square shows the active color and opens the Windows color picker. The compact line-width button opens a wheel underneath for **1–64 px** (click a row, drag or scroll it, use the arrow keys, or type a number and press Enter). The active and custom colors persist.

## Settings

Settings is a compact Windows 11 dialog with tabs on top and its own taskbar button, like the Cloudflare window:

- **General** — capture shortcut, delay, theme (system, light, dark), language (Windows language, English, Türkçe; it applies as soon as you pick it, and Cancel restores the previous one), start with Windows, save notification.
- **Saving** — folder, PNG or JPEG, JPEG quality.
- **Editor** — a 20-color palette plus a custom color, line width 1–64 px, window snapping, close after an action.
- **Sharing** — Cloudflare connection status and the Cloudflare window.
- **Updates** — automatic checks and a manual check with inline progress.

Record a shortcut with Ctrl/Alt/Shift/Win plus a letter, digit, F1–F24 or PrintScreen; bare PrintScreen also works. Recording ends when you click elsewhere; Tab and Enter keep working. A combination already taken by Windows or another app, or one reserved for editing (Alt+F4, Ctrl+C/V/X/Z/Y/A/S, Ctrl+Alt+letter), is reported under the button and the current shortcut is kept. If the shortcut is unavailable at startup, the app offers Ctrl+Shift+S for that session. Controls, scroll bars and menus follow the light or dark theme.

Ctrl+Tab and Ctrl+Shift+Tab switch tabs in Settings and in the Cloudflare window. The tray icon's tooltip shows the capture shortcut. The tray menu offers **Take screenshot** (the default, also on left-click), **Capture last region**, **Settings**, **Screenshots folder**, **Check for updates**, recent captures and **Quit isolmaSS**; opening Settings while it is already open brings that window forward.

## Updates

Update checks read the public releases of this repository, [`isolmaz/isolmaSS-app`](https://github.com/isolmaz/isolmaSS-app/releases). Versions up to 0.6.0 checked a release repository that is no longer public and report *HTTP 404*; install the latest version from the [download page](https://ss.isolmaz.com/download) once and updates work again. A newer release offers **Install / Later / Skip this version**; nothing installs without that choice, and a skipped version is offered again only by a manual check. Installation waits for any open editor, Settings window or running upload, verifies the installer's publisher signature again, closes the app, installs and restarts it. The installer keeps a rollback copy until the new version passes a startup check. A portable copy never installs updates itself: when a newer version exists it offers to open the release page, where you download the new ZIP. The signature is application-level: Windows SmartScreen may still warn, and the app never bypasses that warning. Portable ZIPs are for manual use and do not update an installed copy. See [SECURITY.md](../SECURITY.md) and [DISTRIBUTION.md](../DISTRIBUTION.md).

## Files and privacy

- Screenshots: the Windows Pictures folder → `Screenshots` by default.
- Settings: `%APPDATA%\isolmaSS\settings.json` (at most 64 KiB, validated, saved atomically under a cross-process lock). It holds the Worker address but **no key or token**.
- Worker keys and the optional image password: `%LOCALAPPDATA%\isolmaSS\cloud-credentials.bin`, encrypted for the current Windows user with DPAPI.
- Upload staging: an encoded copy of the selection in the user's temp folder, deleted when the upload card closes.
- Drag staging: the first drag from an upload card copies the uploaded image to `%TEMP%\isolmaSS\drag` so a drop target can read it. The copy is deleted when the card closes (ten seconds after the last accepted drop if that is later), when the app quits and after `--capture-once`. Files in that folder older than a day are swept at startup and after each card closes.
- Text recognition and pins keep their pixels in memory only; nothing is written or sent.
- Diagnostics: `%LOCALAPPDATA%\isolmaSS\logs\diagnostic.log` (rotated near 1 MiB). No pixels, annotation text or keystrokes; errors may include local paths.
- Update downloads: `%LOCALAPPDATA%\isolmaSS\updates`, kept only until the next launch, which deletes installers for the running or older versions and interrupted downloads.

Invalid settings JSON is copied to `settings.corrupt.json` before defaults are used; only that one backup is kept, and older timestamped `settings.corrupt-*` copies are removed. Exported PNG/JPEG and clipboard images are opaque. Save as replaces a file only after encoding and flushing succeed.

## Build and command line

Windows x64, the Windows SDK / Visual Studio C++ Build Tools, Rust **1.98.0** and NSIS are required. `Cargo.lock` pins dependencies.

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1   # fmt, clippy, Worker syntax
cmd /c package.bat
```

`package.bat` verifies versions, size budgets and SHA-256. Set `ISOLMASS_BUILD_DIR=target\release-candidate` while `target\release\isolmass.exe` is running. Local verification is fmt, Clippy and the Worker syntax check; tests run in GitHub Actions, which never packages. `release.bat` needs the publisher's non-exportable signing key. Release steps: [DISTRIBUTION.md](../DISTRIBUTION.md).

Command line: `--capture-once` (editor without the tray; waits for an upload card or a pin to close), `--settings`, `--check-update`, `--verify-update PATH`, `--benchmark N`, `--fix-printscreen` and `--help`. `--smoke-test` needs an interactive desktop and **changes the clipboard**.

Automated tests do not cover the interactive UI. Before a release, check by hand: both themes and both languages, DPI scaling, every tool and export path, tray and Settings flows, updater rollback in an isolated Windows profile, and Cloudflare setup and upload with a real account. MIT license: [LICENSE](../LICENSE); dependencies: [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

