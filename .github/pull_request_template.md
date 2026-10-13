<!-- Written for an automated review and merge agent: be specific, name files, tests and IDs. -->

## Summary

<!-- One or two sentences: what changes and why. -->

## Stack position

- Base: #<n> <!-- or `main` -->
- Next: #<n> <!-- or `none` -->

Merge order: merge with a merge commit, not squash, then delete the branch so the next PR retargets.

## Changes

<!-- Bullet list by area, e.g. overlay, toolbar, save, cloudflare, docs. -->

-

## Findings addressed

<!-- Review finding IDs, one per line; `none` if not applicable. -->

-

## Behavior before → after

<!-- What the user or caller sees, before and after. -->

## Risk and rollback

<!-- What could break, what to check first, and how to revert (revert this merge commit). -->

## Tests added

<!-- Test names, e.g. `overlay::render::tests::name`. Then the CI jobs expected: fmt, clippy, plan, test (<filters>), worker, build. -->

-

CI jobs expected:

## Manual verification

<!-- Numbered steps a reviewer can follow with the `isolmass-<short sha>` artifact from the build job. -->

1. Light theme, 100% scale:
2. Dark theme, 100% scale:
3. Light theme, 150% scale:
4. Dark theme, 150% scale:

## Screenshots

<!-- Links only, demo content only. Never real screenshots, Worker URLs, keys or private paths. -->

## Docs updated

<!-- README.md, docs/GUIDE.md, docs/CLOUDFLARE.md, SECURITY.md, DISTRIBUTION.md, CONTRIBUTING.md: list the files, or `none needed` and why. -->

## Checklist

- [ ] No AI attribution in commit messages or the PR text
- [ ] New or changed interface text has its Turkish translation in `src/i18n/tr.rs`
- [ ] Documentation updated in this PR
- [ ] `scripts\check.ps1` passes locally (fmt, Clippy, Worker syntax); tests ran in CI
