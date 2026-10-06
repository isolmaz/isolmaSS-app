# Contributing

Bug reports and pull requests are welcome.

## Before you open an issue

- **Never attach real screenshots, Worker URLs, keys, `cloud-credentials.bin` or logs with private paths.** Use a demo window or crop.
- Security problems go privately to the address in [SECURITY.md](SECURITY.md), not to an issue.
- Include the version (tray menu or Settings › Updates), Windows version, display scaling and monitor layout.

## Pull requests

1. Keep changes focused; match the surrounding code's naming, comments and idioms.
2. Run the local checks. There is no hosted CI and GitHub Actions is disabled for this repository; every check runs on your machine:

   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts\check.ps1
   ```

   It runs `cargo fmt --check`, Clippy with warnings as errors, the tests and `node --check` on the Worker. Please do not add workflow files.

3. Update the affected documentation in the same change: [README.md](README.md), [docs/GUIDE.md](docs/GUIDE.md), [docs/CLOUDFLARE.md](docs/CLOUDFLARE.md) or [SECURITY.md](SECURITY.md).
4. If the editor, toolbar or upload card changes visibly, mention it so the README media can be re-recorded.
5. Changes to `cloudflare/worker.mjs` that alter behavior must raise `WORKER_VERSION` in both `cloudflare/worker.mjs` and `src/cloudflare_oauth.rs` (a test checks they match) so installed apps offer **Update Worker**.
6. Interface text is written in English inside `crate::i18n::t("…")` or `tf("… {} …", &[…])`, with the literal directly in the call. Add the Turkish translation to `src/i18n/tr.rs`; a test fails when a string is untranslated or a table entry is unused. To add a language, add a table module next to `tr.rs`, a `Language` and `LanguagePreference` variant in `src/i18n.rs`, and an entry in the Settings language list.

Releases and signing are done by the maintainer; see [DISTRIBUTION.md](DISTRIBUTION.md). Contributions are licensed under the [MIT License](LICENSE).
