# Cloudflare sharing

There is **no central isolmaSS image host or sign-in**. Sharing uses a Worker that the app installs in your own Cloudflare account; Copy and Save never upload.

**Setup (first Upload).** The editor steps aside, keeping the capture in memory, and the connection window opens with its own taskbar button. Choose **Continue with Cloudflare**, sign in in the browser and approve the two requested permissions, `workers-scripts.write` and `memberships.read`, for the account you want. Sign-in may take up to 10 minutes. The window comes back to the front; accounts you did not authorize for Workers are hidden, and with a single account installation starts immediately. The app creates a randomly named Worker with a private SQLite Durable Object, enables its `workers.dev` address, protects the generated Worker keys with Windows DPAPI and uploads the pending screenshot. A brand-new `workers.dev` address can take a few minutes to resolve; the pairing is kept meanwhile and uploads work once it answers. No GitHub account, key pasting, R2, D1 or custom domain is needed. The OAuth access token is used only during installation and never stored.

**Cloudflare window.** Once paired it has three tabs and loads current data when opened:

- **Connection** — Worker address, today's and this month's uploads and views, stored images and size, an optional password for new uploads, **Refresh**, **Disconnect** (removes the connection from this computer only; the Worker and images stay) and, when the Worker runs older code than the app bundles, **Update Worker**. Updating asks for Cloudflare consent once more and replaces only the Worker's code; its address, links, keys, images and settings stay.
- **Limits** — images kept (50), daily uploads (20), a daily view level for warnings (1,000), size per image (10 MB) and in total (800 MB), retention (30 days) and the warning threshold (90%). At the threshold choose *warn* or *stop new uploads*. Viewing is never blocked by a quota. Counters cover this Worker only, use UTC days, and **no setting guarantees a zero invoice**.
- **Images** — recent images with local time, size, views and password status; **Open** (or double-click), **Copy link** and **Delete**. Deleted and expired links stop working immediately for new viewers; copies already downloaded, including a viewer's browser cache, cannot be revoked.

A valid link without a password can be viewed by anyone who has it. With a password, only a salted per-image verifier is stored; share the password separately.

## Troubleshooting

**If something goes wrong,** dialogs and the upload card show Cloudflare's own error code and message.

- *Could not check the Cloudflare Workers subdomain (403)*: the chosen account does not grant you Workers access, or the publisher's OAuth client is private (usable only by members of its own account). Pick another account.
- *Your computer could not resolve …*: Windows could not resolve the Worker's host name. The app clears that DNS cache entry and retries automatically; if it persists, check the adapter's DNS servers, especially IPv6 servers on a network without IPv6.
- *401*: the Worker's keys do not match this computer; connect again. The existing Worker is left untouched.
- *429 on upload / 507*: the daily upload limit or the storage limit is reached; wait for the UTC reset, raise the limit or delete images.
- *429 when viewing*: one address opened links more than 60 times in a minute; it can view again a minute later.

## Worker API

`cloudflare/worker.mjs` is the per-user Worker; `cloudflare/wrangler.jsonc` binds `STORE` to its SQLite `ShareStore` Durable Object. `cloudflare/.dev.vars.example` is a local template; never commit real secrets.

- `POST /api/upload` requires the `UPLOAD_TOKEN` bearer secret and PNG/JPEG bytes; optional `X-Image-Password` carries a padding-free base64url UTF-8 password (12–128 printable characters). Success: `201 {"id":"<192-bit id>","url":"https://<worker>/i/<id>"}`.
- `POST /api/setup`, `GET/PUT /api/settings`, `GET /api/stats`, `GET /api/images?limit=50&offset=0` and `DELETE /api/images/:id` require the separate `ADMIN_TOKEN` bearer secret.
- `GET /i/:id` serves the image or a password form, in Turkish when the browser's first preferred language is Turkish and in English otherwise; `POST /i/:id/unlock` checks the password, with at most 20 wrong attempts per hour. Unknown, expired or deleted links return `404`.
- Viewing and unlocking are rate-limited to 60 requests per minute per IP address (`429` with `Retry-After`) through Cloudflare's rate-limiting binding; if an account refuses that binding the Worker runs without it.
- Images without a password are immutable under their ID: responses carry `Cache-Control: public, max-age=31536000, immutable` and an `ETag`, so browsers reuse them and a revalidation returns `304` without touching storage. Password pages and unlocked images use `no-store`. All responses send `nosniff`, CSP and `no-referrer`.
- A view counts once per visitor, image and UTC day. The visitor is a salted, day-specific hash of the IP address; raw addresses are never stored and the hashes are deleted when the day ends.
- `POST /api/setup` and `GET /api/stats` report the Worker version; the app offers **Update Worker** when it bundles newer code.

**Local testing.** Copy `.dev.vars.example` to `.dev.vars` and fill both tokens with 43-character base64url values, for example from `python -c "import secrets; print(secrets.token_urlsafe(32))"`; other formats are rejected with `401`. Then run `npx wrangler@4 dev` in `cloudflare/`. With wrangler 4 the local rate-limiting binding made `/i/*` requests fail with `500`; test with a copy of `wrangler.jsonc` without `ratelimits`.

Images are stored in chunks of at most 1 MB. Consult Cloudflare's [Workers limits](https://developers.cloudflare.com/workers/platform/limits/) and [Durable Objects limits](https://developers.cloudflare.com/durable-objects/platform/limits/); free allowances are shared by the whole account.

## Website

`ss.isolmaz.com` is a static information and download site maintained outside this repository. It hosts no screenshots, installs no Workers and links to the [latest signed release](https://github.com/isolmaz/isolmaSS-app/releases/latest).
