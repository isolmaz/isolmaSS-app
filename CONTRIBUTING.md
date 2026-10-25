# Contributing

Bug reports and pull requests are welcome.

## Before you open an issue

- **Never attach real screenshots, Worker URLs, keys, `cloud-credentials.bin` or logs with private paths.** Use a demo window or crop.
- Security problems go privately to the address in [SECURITY.md](SECURITY.md), not to an issue.
- Include the version (Settings › Updates), Windows version, display scaling and monitor layout.

## Pull requests

1. Keep changes focused; match the surrounding code's naming, comments and idioms.
2. Run the local checks. Tests are **not** run on your machine; GitHub Actions runs them on every pull request:

   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts\check.ps1
   ```

   Locally this is only `cargo fmt --check`, Clippy with warnings as errors (`cargo clippy --all-targets --locked -- -D warnings`) and `node --check cloudflare/worker.mjs`.

   CI (`.github/workflows/ci.yml`, on pushes to `main` and pull requests against any branch) runs small separate jobs: `fmt`, `clippy`, `worker`, a `build` job that uploads an unsigned `isolmass-<short sha>` artifact for reviewers, and `test`. `scripts/ci-test-plan.ps1` picks the `test` jobs from the files the pull request changes: `src/<module>.rs` or `src/<module>/…` runs the `<module>::` tests, any changed Rust source also runs `i18n::` (it scans all sources), and changes to `Cargo.toml`, `Cargo.lock`, `build.rs`, `rust-toolchain.toml`, `resources/`, `src/main.rs`, `src/theme.rs`, `src/drawing.rs`, the workflows or the plan script run every module. Changes under `cloudflare/` run the `worker` job; a change to `cloudflare/worker.mjs` also runs `cloudflare_oauth::` and `cloud_settings_window::`, which test the source the app embeds. The workflows must stay free of secrets, packaging, signing and release steps.

   If a test fails in CI, re-run just that test locally by name:

   ```powershell
   cargo test --locked <test_name> -- --test-threads=1
   ```

3. Update the affected documentation in the same change: [README.md](README.md), [docs/GUIDE.md](docs/GUIDE.md), [docs/CLOUDFLARE.md](docs/CLOUDFLARE.md) or [SECURITY.md](SECURITY.md).
4. If the editor, toolbar or upload card changes visibly, mention it so the README media can be re-recorded.
5. Changes to `cloudflare/worker.mjs` that alter behavior must raise `WORKER_VERSION` in both `cloudflare/worker.mjs` and `src/cloudflare_oauth.rs` (a test checks they match) so installed apps offer **Update Worker**.
6. New code keeps the 100% line coverage of the Release check. Put decisions, geometry, parsing and drawing in covered modules and test them headlessly (render into a memory DC, encode into a temp folder, talk to a loopback server). Only code that needs a live desktop, a real user or a live internet service goes into a glue file listed with a reason in `scripts/coverage-exclude.txt`. The gate counts every closure and every generic instantiation separately: a `.map_err(|e| …)` that never runs is a missed line.
7. Interface text is written in English inside `crate::i18n::t("…")` or `tf("… {} …", &[…])`, with the literal directly in the call; this includes error messages the user can see. Show dialogs through `ui::error`, `ui::info` and `ui::confirm` so their buttons follow the app language, and never branch on translated text. Add the Turkish translation to `src/i18n/tr.rs`; a test fails when a string is untranslated or a table entry is unused. To add a language, add a table module next to `tr.rs`, a `Language` and `LanguagePreference` variant in `src/i18n.rs`, and an entry in the Settings language list.

Stacked pull requests (each based on the previous one) are merged in order with a merge commit, not squash; delete the merged branch so the next pull request retargets. Fill in the pull request template.

Before every release the maintainer runs the **Release check** workflow (`.github/workflows/release-check.yml`: the full test suite and 100% line coverage, with the excluded files listed in `scripts/coverage-exclude.txt`, plus the Worker coverage). Releases and signing are done by the maintainer; see [DISTRIBUTION.md](DISTRIBUTION.md). Contributions are licensed under the [MIT License](LICENSE).
