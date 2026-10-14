import { DurableObject } from 'cloudflare:workers';

/** Reported by /api/setup and /api/stats so the app can offer a Worker update. */
const WORKER_VERSION = 5;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const IMAGE_ID = /^[A-Za-z0-9_-]{32}$/;
/** `/i/<id>` or `/i/<id>.png|.jpg`; the extension addresses the raw bytes. */
const SHARE_NAME = /^([A-Za-z0-9_-]{32})(?:\.(png|jpg))?$/;
const UNLOCK_SECONDS = 3600;
const PASSWORD_ATTEMPTS_PER_HOUR = 20;
const ROBOTS = 'noindex, nofollow, noimageindex';
const CHUNK_BYTES = 1_000_000;
const FREE_STORAGE_BYTES = 800_000_000;
const SETTINGS = {
  max_active: [1, 5000],
  max_image_bytes: [1024, 10 * 1024 * 1024],
  max_storage_bytes: [1_048_576, FREE_STORAGE_BYTES],
  daily_upload_limit: [1, 1000],
  daily_view_limit: [1, 20_000],
  retention_days: [1, 3650],
  warning_percent: [1, 100],
};
const SECURITY = {
  'Referrer-Policy': 'no-referrer',
  'X-Content-Type-Options': 'nosniff',
  'Content-Security-Policy': "default-src 'none'; frame-ancestors 'none'",
};
const NO_STORE = { ...SECURITY, 'Cache-Control': 'no-store, max-age=0' };
/** Headers of everything under /i/: share links must never be indexed. */
const SHARE_HEADERS = { ...SECURITY, 'X-Robots-Tag': ROBOTS };

function json(value, status = 200, extra = {}) {
  return new Response(JSON.stringify(value), {
    status, headers: { ...NO_STORE, 'Content-Type': 'application/json; charset=utf-8', ...extra },
  });
}
/** Answers HEAD like GET, without the body. */
function respond(request, body, init) {
  return new Response(request.method === 'HEAD' ? null : body, init);
}
function etag(id) { return `"${id}"`; }
function extensionOf(record) { return record.content_type === 'image/png' ? 'png' : 'jpg'; }
/** Dated name for downloads, in UTC: isolmaSS-2026-10-14_09-41.png. */
function shareFilename(record) {
  const stamp = new Date(record.created_at * 1000).toISOString().slice(0, 16).replace('T', '_').replace(':', '-');
  return `isolmaSS-${stamp}.${extensionOf(record)}`;
}
/**
 * Public images never change under their random ID, but they expire and can be
 * deleted, so a browser may keep one only until the expiry and no shared cache
 * may keep it at all.
 */
function imageCaching(record, now) {
  return {
    'Cache-Control': `private, max-age=${record.expires_at - Math.floor(now / 1000)}, immutable`,
    ETag: etag(record.id),
    Vary: 'Accept, Sec-Fetch-Dest',
  };
}
function notModified(record, now) {
  return new Response(null, { status: 304, headers: { ...SHARE_HEADERS, ...imageCaching(record, now) } });
}
/** Top-level navigations get the HTML page; embeds and unfurlers get the bytes. */
function wantsPage(request) {
  const destination = request.headers.get('Sec-Fetch-Dest');
  if (destination !== null) return destination === 'document';
  return (request.headers.get('Accept') || '').includes('text/html');
}
async function visitorKey(request, salt, day) {
  const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
  const digest = await crypto.subtle.digest('SHA-256', encoder.encode(`${salt}|${day}|${ip}`));
  return encodeBase64Url(new Uint8Array(digest).subarray(0, 16));
}
function error(status, message, extra = {}) { return json({ error: message }, status, extra); }
class LimitStop extends Error {
  constructor(response) { super('Upload limit reached.'); this.response = response; }
}
/** A body over its limit: 413, not the 400 of other malformed input. */
class TooLarge extends RangeError {}
function dayOf(now) { return new Date(now).toISOString().slice(0, 10); }
function monthOf(now) { return dayOf(now).slice(0, 7); }
/** Today's upload count has reached the hard cap or the stop-new-uploads threshold. */
function uploadsExhausted(prefs, daily) {
  return daily.uploads >= 1000 || (prefs.limit_action === 'block_upload' &&
    daily.uploads >= Math.max(1, Math.floor(prefs.daily_upload_limit * prefs.warning_percent / 100)));
}
/** The counter resets at the next UTC midnight; the app turns Retry-After into a local time. */
function uploadsLimited(now) {
  const seconds = Math.ceil((86_400_000 - now % 86_400_000) / 1000);
  return error(429, 'Upload limit reached. Change limits or wait for reset.', { 'Retry-After': String(seconds) });
}
function randomId() {
  return btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(24))))
    .replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
}
function encodeBase64Url(bytes) {
  return btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
}
function decodeBase64Url(value) {
  const base = value.replaceAll('-', '+').replaceAll('_', '/');
  return Uint8Array.from(atob(base.padEnd(Math.ceil(base.length / 4) * 4, '=')), ch => ch.charCodeAt(0));
}
function equalBytes(left, right) {
  if (left.length !== right.length) return false;
  let different = 0;
  for (let i = 0; i < left.length; i++) different |= left[i] ^ right[i];
  return different === 0;
}
async function authorized(request, secret) {
  const header = request.headers.get('Authorization') || '';
  if (!secret || !/^Bearer [A-Za-z0-9_-]{43}$/.test(header)) return false;
  const [left, right] = await Promise.all([
    crypto.subtle.digest('SHA-256', encoder.encode(header.slice(7))),
    crypto.subtle.digest('SHA-256', encoder.encode(secret)),
  ]);
  return equalBytes(new Uint8Array(left), new Uint8Array(right));
}
function originFor(request, env) {
  const url = new URL(request.url);
  if (url.protocol !== 'https:' && !['localhost', '127.0.0.1'].includes(url.hostname))
    throw new Error('HTTPS required.');
  if (env.PUBLIC_ORIGIN && new URL(env.PUBLIC_ORIGIN).origin !== url.origin)
    throw new Error('Unexpected hostname.');
  return url.origin;
}
async function limitedBody(request, maximum) {
  if (!request.body) throw new RangeError('Missing request body.');
  const reader = request.body.getReader();
  const parts = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maximum) { await reader.cancel(); throw new TooLarge('Request body is too large.'); }
      parts.push(value);
    }
  } finally { reader.releaseLock(); }
  const result = new Uint8Array(total);
  let position = 0;
  for (const part of parts) { result.set(part, position); position += part.byteLength; }
  return result;
}
function imagePassword(request) {
  const raw = request.headers.get('X-Image-Password');
  if (raw === null) return null;
  if (!/^[A-Za-z0-9_-]{16,172}$/.test(raw)) throw new RangeError('Invalid image password.');
  let password;
  try { password = decoder.decode(decodeBase64Url(raw)); }
  catch { throw new RangeError('Invalid image password.'); }
  if (password.length < 12 || password.length > 128 || /[\x00-\x1f\x7f]/.test(password))
    throw new RangeError('Image password must contain 12–128 printable characters.');
  return password;
}
async function digestPassword(password, salt) {
  const key = await crypto.subtle.importKey('raw', encoder.encode(password), 'PBKDF2', false, ['deriveBits']);
  return new Uint8Array(await crypto.subtle.deriveBits(
    { name: 'PBKDF2', hash: 'SHA-256', salt, iterations: 30_000 }, key, 256,
  ));
}
async function screenshot(request, maximum, type) {
  if (!request.body) throw new RangeError('No screenshot was sent.');
  const lengthHeader = request.headers.get('Content-Length');
  if (lengthHeader && (!/^\d+$/.test(lengthHeader) || Number(lengthHeader) > maximum))
    throw new TooLarge('Image exceeds the configured size limit.');
  const reader = request.body.getReader();
  const chunks = [];
  const prefix = new Uint8Array(24);
  let prefixCount = 0, total = 0, current = new Uint8Array(CHUNK_BYTES), used = 0;
  let lastByte = 0, secondLastByte = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maximum) { await reader.cancel(); throw new TooLarge('Image exceeds the configured size limit.'); }
      if (prefixCount < 24) {
        const count = Math.min(value.byteLength, 24 - prefixCount);
        prefix.set(value.subarray(0, count), prefixCount);
        prefixCount += count;
      }
      if (value.byteLength >= 2) {
        secondLastByte = value[value.byteLength - 2];
        lastByte = value[value.byteLength - 1];
      } else if (value.byteLength === 1) {
        secondLastByte = lastByte;
        lastByte = value[0];
      }
      for (let offset = 0; offset < value.byteLength;) {
        const count = Math.min(CHUNK_BYTES - used, value.byteLength - offset);
        current.set(value.subarray(offset, offset + count), used);
        used += count;
        offset += count;
        if (used === CHUNK_BYTES) { chunks.push(current); current = new Uint8Array(CHUNK_BYTES); used = 0; }
      }
    }
  } finally { reader.releaseLock(); }
  if (total < 24) throw new RangeError('Incomplete image.');
  let width = null, height = null;
  if (type === 'image/png') {
    if (!equalBytes(prefix.subarray(0, 8), Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10)) ||
      prefix[12] !== 73 || prefix[13] !== 72 || prefix[14] !== 68 || prefix[15] !== 82)
      throw new RangeError('Invalid PNG image.');
    const view = new DataView(prefix.buffer);
    width = view.getUint32(16);
    height = view.getUint32(20);
    if (!width || !height || width > 16384 || height > 16384 || width * height > 100_000_000)
      throw new RangeError('Image dimensions exceed the limit.');
  } else if (prefix[0] !== 255 || prefix[1] !== 216 || prefix[2] !== 255 ||
    secondLastByte !== 255 || lastByte !== 217) {
    throw new RangeError('Invalid JPEG image.');
  }
  if (used) chunks.push(current.slice(0, used));
  return { chunks, total, width, height };
}
/** Texts of every page a viewer can see; each page is a { title, heading, note }. */
const VIEWER_TEXT = {
  en: {
    footer: 'Shared with {brand} · Free screenshot tool for Windows',
    cta: 'Get isolmaSS — free',
    label: 'Password',
    button: 'Open screenshot',
    wrong: 'Wrong password. Check it and try again.',
    shot: {
      title: 'Screenshot', alt: 'Shared screenshot', download: 'Download', original: 'Open original',
      copy: 'Copy link', copied: 'Copied', copyFailed: 'Copy failed', report: 'Report', reportTitle: 'Report abuse to Cloudflare',
    },
    password: { title: 'Password-protected screenshot', heading: 'This screenshot is password protected', note: 'Ask the person who shared it for the password.' },
    gone: { title: 'Screenshot unavailable', heading: 'This screenshot is no longer available', note: 'It expired or its owner deleted it.' },
    tooMany: { title: 'Too many attempts', heading: 'Too many attempts', note: 'Try again in one hour.' },
    busy: { title: 'Too many requests', heading: 'Too many requests', note: 'Try again in a minute.' },
    invalid: { title: 'Request not supported', heading: 'Request not supported', note: 'This request could not be processed.' },
    unavailable: { title: 'Temporarily unavailable', heading: 'Temporarily unavailable', note: 'Try again in a minute.' },
    notFound: { title: 'Page not found', heading: 'Page not found', note: 'Check the address and try again.' },
    home: { title: 'Private share host', heading: 'Private isolmaSS share host', note: 'Screenshots here can only be opened with their link.' },
  },
  tr: {
    footer: '{brand} ile paylaşıldı · Windows için ücretsiz ekran görüntüsü aracı',
    cta: "isolmaSS'i ücretsiz edinin",
    label: 'Şifre',
    button: 'Ekran görüntüsünü aç',
    wrong: 'Şifre yanlış. Kontrol edip yeniden deneyin.',
    shot: {
      title: 'Ekran görüntüsü', alt: 'Paylaşılan ekran görüntüsü', download: 'İndir', original: 'Orijinali aç',
      copy: 'Bağlantıyı kopyala', copied: 'Kopyalandı', copyFailed: 'Kopyalanamadı', report: 'Bildir', reportTitle: "Kötüye kullanımı Cloudflare'e bildir",
    },
    password: { title: 'Şifreli ekran görüntüsü', heading: 'Bu ekran görüntüsü şifreli', note: 'Şifreyi paylaşan kişiden isteyin.' },
    gone: { title: 'Ekran görüntüsü yok', heading: 'Bu ekran görüntüsü artık yok', note: 'Süresi doldu ya da paylaşan kişi sildi.' },
    tooMany: { title: 'Çok fazla deneme', heading: 'Çok fazla deneme', note: 'Bir saat sonra yeniden deneyin.' },
    busy: { title: 'Çok fazla istek', heading: 'Çok fazla istek', note: 'Bir dakika sonra yeniden deneyin.' },
    invalid: { title: 'İstek desteklenmiyor', heading: 'İstek desteklenmiyor', note: 'Bu istek işlenemedi.' },
    unavailable: { title: 'Geçici olarak kullanılamıyor', heading: 'Geçici olarak kullanılamıyor', note: 'Bir dakika sonra yeniden deneyin.' },
    notFound: { title: 'Sayfa bulunamadı', heading: 'Sayfa bulunamadı', note: 'Adresi kontrol edip yeniden deneyin.' },
    home: { title: 'Özel paylaşım sunucusu', heading: 'Özel isolmaSS paylaşım sunucusu', note: 'Buradaki ekran görüntüleri yalnızca bağlantısıyla açılabilir.' },
  },
};
/** Turkish for browsers that prefer it, otherwise English. */
function viewerLanguage(request) {
  const first = (request.headers.get('Accept-Language') || '').split(',')[0].trim().toLowerCase();
  return first.startsWith('tr') ? 'tr' : 'en';
}
const HTML_ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
function escapeHtml(value) { return String(value).replace(/[&<>"']/g, ch => HTML_ESCAPES[ch]); }

/** The app icon, also used as the favicon of every page. */
const ICON_SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" fill="none"><rect x="16" y="16" width="224" height="224" rx="52" fill="#F59E0B"/><path d="M184 104V76C184 64.9543 175.046 56 164 56H76C64.9543 56 56 64.9543 56 76V164C56 175.046 64.9543 184 76 184H104" stroke="#141413" stroke-width="22" stroke-linecap="round" stroke-linejoin="round"/><path d="M120 120L210 150L168 168L150 210L120 120Z" fill="#FFFFFF" stroke="#141413" stroke-width="12" stroke-linejoin="round"/></svg>';
const FAVICON_URI = `data:image/svg+xml,${encodeURIComponent(ICON_SVG)}`;
const BRAND_MARK = ICON_SVG.replace('<svg ', '<svg width="22" height="22" aria-hidden="true" ');
const SITE = 'https://ss.isolmaz.com';
const DOWNLOAD_PAGE = `${SITE}/download`;
/** The screenshots live in the uploader's own Cloudflare account, so Cloudflare is who takes abuse reports. */
const ABUSE_FORM = 'https://abuse.cloudflare.com/';
/** Shows the "Copy link" button where the Clipboard API exists; a copy or a refusal is confirmed on it for two seconds. */
const COPY_SCRIPT = "(()=>{const b=document.getElementById('copy');if(!navigator.clipboard)return;b.hidden=false;const label=b.textContent;let timer;const show=text=>{b.textContent=text;clearTimeout(timer);timer=setTimeout(()=>{b.textContent=label},2000)};b.onclick=()=>navigator.clipboard.writeText(b.dataset.url).then(()=>show(b.dataset.done)).catch(()=>show(b.dataset.failed))})()";
const GLYPHS = {
  lock: '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/></svg>',
  alert: '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M12 7v6"/><path d="M12 16.5v.01"/></svg>',
};
/** Windows 11 look: Segoe UI Variable, Fluent neutrals, 4 px controls, 8 px cards, amber only as the accent. */
const PAGE_CSS = `
:root{--bg:#F3F3F3;--card:#FFFFFF;--fg:#1A1A1A;--muted:#5D5D5D;--line:#E5E5E5;--accent:#F59E0B;--accent-hover:#F7AD33;--accent-pressed:#D98A06;--ink:#141413;--link:#B45309;--danger:#C42B1C}
@media (prefers-color-scheme:dark){:root{--bg:#202020;--card:#2B2B2B;--fg:#FFFFFF;--muted:#C5C5C5;--line:#3A3A3A;--link:#FBBF24;--danger:#FF99A4}}
*{box-sizing:border-box}
body{margin:0;min-height:100dvh;display:flex;flex-direction:column;background:var(--bg);color:var(--fg);font:16px/1.5 "Segoe UI Variable Text","Segoe UI",system-ui,sans-serif}
main{margin:auto}
.card{width:min(26rem,100% - 2rem);padding:2rem;background:var(--card);border:1px solid var(--line);border-radius:8px;box-shadow:0 2px 4px rgba(0,0,0,.08)}
.glyph{display:grid;place-items:center;width:2.5rem;height:2.5rem;margin-bottom:1rem;border-radius:8px;background:var(--accent);color:var(--ink)}
h1{margin:0 0 .5rem;font-size:1.25rem;font-weight:600;line-height:1.3}
.note{margin:0;color:var(--muted)}
form{display:grid;gap:.75rem;margin-top:1.5rem}
label{font-size:.875rem;font-weight:600}
input{width:100%;min-height:2.25rem;padding:.4rem .75rem;border:1px solid var(--line);border-bottom-color:var(--muted);border-radius:4px;background:var(--card);color:var(--fg);font:inherit}
input[aria-invalid=true]{border-color:var(--danger)}
.error{margin:0;color:var(--danger);font-size:.875rem}
.btn{display:inline-flex;align-items:center;justify-content:center;min-height:2.25rem;padding:.35rem 1.25rem;border:1px solid var(--line);border-radius:4px;background:var(--card);color:var(--fg);font:inherit;font-weight:600;text-decoration:none;cursor:pointer}
.btn:hover{background:var(--bg)}
.btn.primary{border-color:transparent;background:var(--accent);color:var(--ink)}
.btn.primary:hover{background:var(--accent-hover)}
.btn.primary:active{background:var(--accent-pressed)}
.btn.quiet{border-color:transparent;background:transparent;color:var(--muted);font-weight:400}
.btn.quiet:hover{background:var(--card);color:var(--fg)}
.btn[hidden]{display:none}
:focus-visible{outline:2px solid var(--fg);outline-offset:2px}
a{color:var(--link)}
.top{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.5rem 1rem;padding:.5rem 1rem;border-bottom:1px solid var(--line);background:var(--card)}
.brand{display:inline-flex;align-items:center;gap:.5rem;color:var(--fg);font-weight:600;text-decoration:none}
.brand svg{display:block}
.top .btn{min-height:2rem;padding:.2rem 1rem;font-size:.875rem}
.bar{display:flex;flex-wrap:wrap;gap:.5rem;padding:.75rem 1rem}
.end{margin-inline-start:auto}
.stage{display:grid;place-items:center;flex:1;margin:0;padding:0 1rem 1rem}
.shot{display:block;cursor:zoom-in}
.shot img{display:block;width:auto;height:auto;max-width:100%;max-height:calc(100dvh - 12rem);object-fit:contain;border:1px solid var(--line);border-radius:8px;background:var(--card)}
footer{padding:1rem;text-align:center;color:var(--muted);font-size:.875rem}
`.replaceAll('\n', '');
const PAGE_CSP = "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

function documentShell(lang, { title, head, body }) {
  const text = VIEWER_TEXT[lang];
  return `<!doctype html><html lang="${lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><meta name="robots" content="${ROBOTS}"><meta name="theme-color" content="#F3F3F3" media="(prefers-color-scheme: light)"><meta name="theme-color" content="#202020" media="(prefers-color-scheme: dark)"><title>${escapeHtml(title)} · isolmaSS</title><link rel="icon" type="image/svg+xml" href="${FAVICON_URI}"><meta property="og:site_name" content="isolmaSS">${head}<style>${PAGE_CSS}</style></head><body><header class="top"><a class="brand" href="${SITE}">${BRAND_MARK}isolmaSS</a><a class="btn primary" href="${DOWNLOAD_PAGE}">${text.cta}</a></header>${body}<footer>${text.footer.replace('{brand}', `<a href="${SITE}">isolmaSS</a>`)}</footer></body></html>`;
}
/** `nonce` allows the one inline script of the share page; every other page runs none. */
function htmlResponse(request, html, { status = 200, form = false, nonce = '', headers = {} } = {}) {
  return respond(request, html, { status, headers: {
    ...NO_STORE, ...SHARE_HEADERS, 'Content-Type': 'text/html; charset=utf-8',
    'Content-Security-Policy': `${PAGE_CSP}${nonce ? `; script-src 'nonce-${nonce}'` : ''}; form-action ${form ? "'self'" : "'none'"}`,
    'X-Frame-Options': 'DENY', ...headers,
  } });
}
/** One centered card: a glyph, heading, note and optionally a form. */
function cardPage(request, kind, { status = 200, glyph = 'alert', form = '', headers }) {
  const lang = viewerLanguage(request), text = VIEWER_TEXT[lang][kind];
  const html = documentShell(lang, {
    title: text.title,
    head: `<meta property="og:title" content="${escapeHtml(text.title)}"><meta property="og:description" content="${escapeHtml(text.note)}">`,
    body: `<main class="card"><div class="glyph">${GLYPHS[glyph]}</div><h1>${text.heading}</h1><p class="note">${text.note}</p>${form}</main>`,
  });
  return htmlResponse(request, html, { status, form: Boolean(form), headers });
}
/** An error on a viewer-facing page, in the viewer's language. */
function viewerError(request, status, kind, headers = {}) {
  return cardPage(request, kind, { status, headers });
}
function passwordPage(request, id, wrong = false) {
  const text = VIEWER_TEXT[viewerLanguage(request)];
  const form = `<form method="post" action="/i/${id}/unlock"><label for="password">${text.label}</label><input id="password" name="password" type="password" required autofocus minlength="12" maxlength="128" autocomplete="off" autocapitalize="off" spellcheck="false"${wrong ? ' aria-invalid="true" aria-describedby="error"' : ''}>${wrong ? `<p class="error" id="error" role="alert">${text.wrong}</p>` : ''}<button class="btn primary" type="submit">${text.button}</button></form>`;
  return cardPage(request, 'password', { status: wrong ? 403 : 200, glyph: 'lock', form, headers: { Vary: 'Cookie' } });
}
/**
 * The page a share shows to a browser; the image itself is the raw URL. A password-protected
 * share shows it once unlocked, with `?raw=1` as the raw URL because the unlock cookie does not
 * reach the .png address, and without the og: tags, which no unfurler could fetch.
 */
function sharePage(request, record, locked) {
  const lang = viewerLanguage(request), text = VIEWER_TEXT[lang].shot;
  const id = record.id, origin = new URL(request.url).origin;
  const raw = locked ? `/i/${id}?raw=1` : `/i/${id}.${extensionOf(record)}`;
  const absolute = escapeHtml(`${origin}${raw}`);
  const known = record.width && record.height;
  const nonce = randomId();
  const html = documentShell(lang, {
    title: text.title,
    head: locked ? '' : `<meta property="og:type" content="website"><meta property="og:title" content="${escapeHtml(text.title)} · isolmaSS"><meta property="og:image" content="${absolute}"><meta property="og:image:type" content="${record.content_type}">${known ? `<meta property="og:image:width" content="${record.width}"><meta property="og:image:height" content="${record.height}">` : ''}<meta name="twitter:card" content="summary_large_image"><meta name="twitter:image" content="${absolute}">`,
    body: `<div class="bar"><a class="btn" href="/i/${id}?download=1">${text.download}</a><a class="btn" href="${raw}" target="_blank" rel="noopener">${text.original}</a><button class="btn" id="copy" type="button" hidden aria-live="polite" data-url="${escapeHtml(`${origin}/i/${id}`)}" data-done="${text.copied}" data-failed="${text.copyFailed}">${text.copy}</button><a class="btn quiet end" href="${ABUSE_FORM}" target="_blank" rel="noopener noreferrer" title="${text.reportTitle}" aria-label="${text.reportTitle}">${text.report}</a></div><main class="stage"><a class="shot" href="${raw}"><img src="${raw}" alt="${text.alt}"${known ? ` width="${record.width}" height="${record.height}"` : ''}></a></main><script nonce="${nonce}">${COPY_SCRIPT}</script>`,
  });
  return htmlResponse(request, html, { nonce, headers: locked ? { Vary: 'Cookie' } : {} });
}
/** Cheap edge answers that never need the Durable Object or the rate limiter. */
function edgeReply(request, path) {
  const stored = path === '/favicon.ico' || path === '/favicon.svg' || path === '/robots.txt' || path === '/';
  if (!stored) return path.startsWith('/i/') ? null : viewerError(request, 404, 'notFound');
  if (request.method !== 'GET' && request.method !== 'HEAD') return viewerError(request, 405, 'invalid', { Allow: 'GET, HEAD' });
  if (path === '/robots.txt') {
    return respond(request, 'Not found.', { status: 404, headers: { ...SECURITY, 'Cache-Control': 'public, max-age=86400', 'Content-Type': 'text/plain; charset=utf-8' } });
  }
  if (path === '/') return cardPage(request, 'home', { headers: { 'Cache-Control': 'public, max-age=3600', Vary: 'Accept-Language' } });
  return respond(request, ICON_SVG, { headers: { ...SECURITY, 'Cache-Control': 'public, max-age=604800', 'Content-Type': 'image/svg+xml' } });
}

export class ShareStore extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`
      CREATE TABLE IF NOT EXISTS preferences (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        max_active INTEGER NOT NULL DEFAULT 50,
        max_image_bytes INTEGER NOT NULL DEFAULT 10485760,
        max_storage_bytes INTEGER NOT NULL DEFAULT 800000000,
        daily_upload_limit INTEGER NOT NULL DEFAULT 20,
        daily_view_limit INTEGER NOT NULL DEFAULT 1000,
        retention_days INTEGER NOT NULL DEFAULT 30,
        warning_percent INTEGER NOT NULL DEFAULT 90,
        limit_action TEXT NOT NULL DEFAULT 'warn'
      );
      INSERT OR IGNORE INTO preferences(id) VALUES (1);
      CREATE TABLE IF NOT EXISTS images (
        id TEXT PRIMARY KEY,
        content_type TEXT NOT NULL,
        size_bytes INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        password_salt TEXT,
        password_hash TEXT,
        views INTEGER NOT NULL DEFAULT 0
      );
      CREATE INDEX IF NOT EXISTS images_created ON images(created_at, id);
      CREATE INDEX IF NOT EXISTS images_expires ON images(expires_at);
      CREATE TABLE IF NOT EXISTS chunks (
        image_id TEXT NOT NULL,
        part INTEGER NOT NULL,
        bytes BLOB NOT NULL,
        PRIMARY KEY(image_id, part)
      );
      CREATE TABLE IF NOT EXISTS usage_daily (
        day TEXT PRIMARY KEY,
        uploads INTEGER NOT NULL DEFAULT 0,
        views INTEGER NOT NULL DEFAULT 0,
        bytes_uploaded INTEGER NOT NULL DEFAULT 0
      );
      CREATE TABLE IF NOT EXISTS password_attempts (
        id TEXT NOT NULL,
        hour INTEGER NOT NULL,
        failures INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(id, hour)
      );
      CREATE TABLE IF NOT EXISTS view_marks (
        day TEXT NOT NULL,
        image_id TEXT NOT NULL,
        visitor TEXT NOT NULL,
        PRIMARY KEY(day, image_id, visitor)
      );
      CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );
    `);
    // Viewing is never blocked by a quota; older installs may still say block_all.
    this.sql.exec("UPDATE preferences SET limit_action='block_upload' WHERE limit_action='block_all'");
    // CREATE TABLE IF NOT EXISTS never adds columns to a table an older Worker created.
    const columns = this.sql.exec('PRAGMA table_info(images)').toArray().map(column => column.name);
    for (const column of ['width', 'height'])
      if (!columns.includes(column)) this.sql.exec(`ALTER TABLE images ADD COLUMN ${column} INTEGER`);
    this.ensureMeta('visitor_salt', randomId);
    this.ensureMeta('unlock_key', () => encodeBase64Url(crypto.getRandomValues(new Uint8Array(32))));
  }

  ensureMeta(key, create) {
    if (!this.sql.exec('SELECT value FROM meta WHERE key=?', key).toArray().length)
      this.sql.exec('INSERT INTO meta(key, value) VALUES(?, ?)', key, create());
  }
  meta(key) { return this.sql.exec('SELECT value FROM meta WHERE key=?', key).one().value; }
  visitorSalt() { return this.meta('visitor_salt'); }
  /** Signs and verifies the "password already entered" cookie of one share. */
  async unlockSignature(id, expires) {
    const key = await crypto.subtle.importKey('raw', decodeBase64Url(this.meta('unlock_key')), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
    return new Uint8Array(await crypto.subtle.sign('HMAC', key, encoder.encode(`${id}|${expires}`)));
  }
  async unlockCookie(id, now) {
    const expires = Math.floor(now / 1000) + UNLOCK_SECONDS;
    const signature = encodeBase64Url(await this.unlockSignature(id, expires));
    return `u=${expires}.${signature}; Path=/i/${id}; Max-Age=${UNLOCK_SECONDS}; HttpOnly; Secure; SameSite=Lax`;
  }
  async unlocked(request, id, now) {
    const sent = /(?:^|;\s*)u=(\d{1,12})\.([A-Za-z0-9_-]{43})(?:;|$)/.exec(request.headers.get('Cookie') || '');
    if (!sent || Number(sent[1]) <= Math.floor(now / 1000)) return false;
    return equalBytes(await this.unlockSignature(id, sent[1]), decodeBase64Url(sent[2]));
  }
  pruneViews(now) { this.sql.exec('DELETE FROM view_marks WHERE day < ?', dayOf(now)); }

  settings() { return this.sql.exec('SELECT * FROM preferences WHERE id = 1').one(); }
  daily(now) {
    return this.sql.exec('SELECT uploads, views, bytes_uploaded FROM usage_daily WHERE day = ?', dayOf(now)).toArray()[0]
      || { uploads: 0, views: 0, bytes_uploaded: 0 };
  }
  stored() {
    return this.sql.exec('SELECT COUNT(*) AS active, COALESCE(SUM(size_bytes),0) AS stored_bytes FROM images').one();
  }
  remove(id) {
    this.sql.exec('DELETE FROM chunks WHERE image_id = ?', id);
    this.sql.exec('DELETE FROM password_attempts WHERE id = ?', id);
    this.sql.exec('DELETE FROM images WHERE id = ?', id);
  }
  trim(now, limit) {
    const obsolete = this.sql.exec('SELECT id FROM images WHERE expires_at <= ? ORDER BY expires_at', Math.floor(now / 1000)).toArray();
    for (const row of obsolete) this.remove(row.id);
    const overflow = this.sql.exec('SELECT id FROM images ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?', limit).toArray();
    for (const row of overflow) this.remove(row.id);
  }
  async nextAlarm() {
    const earliest = this.sql.exec('SELECT MIN(expires_at) AS value FROM images').one().value;
    if (earliest === null) await this.ctx.storage.deleteAlarm();
    else await this.ctx.storage.setAlarm(earliest * 1000);
  }
  async alarm() {
    this.ctx.storage.transactionSync(() => {
      this.trim(Date.now(), this.settings().max_active);
      this.pruneViews(Date.now());
      this.sql.exec('DELETE FROM password_attempts WHERE hour < ?', Math.floor(Date.now() / 3_600_000) - 24);
    });
    await this.nextAlarm();
  }
  async upload(request, now, origin) {
    const settings = this.settings();
    const type = request.headers.get('Content-Type');
    if (type !== 'image/png' && type !== 'image/jpeg') throw new RangeError('Only PNG and JPEG are accepted.');
    const password = imagePassword(request);
    // A user on a slow uplink should not have to send the whole image to learn the day's uploads are used up.
    if (uploadsExhausted(settings, this.daily(now))) return uploadsLimited(now);
    const { chunks, total, width, height } = await screenshot(request, settings.max_image_bytes, type);
    const salt = password ? crypto.getRandomValues(new Uint8Array(16)) : null;
    const hash = salt ? await digestPassword(password, salt) : null;
    const id = randomId(), nowSeconds = Math.floor(now / 1000);
    try {
      this.ctx.storage.transactionSync(() => {
      const prefs = this.settings();
      this.trim(now, prefs.max_active - 1);
      const daily = this.daily(now);
      const occupied = this.stored().stored_bytes;
      const threshold = prefs.warning_percent / 100;
      this.pruneViews(now);
      if (uploadsExhausted(prefs, daily)) throw new LimitStop(uploadsLimited(now));
      if (prefs.limit_action === 'block_upload' && occupied + total > prefs.max_storage_bytes * threshold) {
        throw new LimitStop(error(429, 'Upload limit reached. Change limits or wait for reset.'));
      }
      if (occupied + total > prefs.max_storage_bytes) {
        throw new LimitStop(error(507, 'Free storage limit reached. Delete images or lower retention.'));
      }
      this.sql.exec('INSERT INTO images(id, content_type, size_bytes, created_at, expires_at, password_salt, password_hash, width, height) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)',
        id, type, total, nowSeconds, nowSeconds + prefs.retention_days * 86400,
        salt ? encodeBase64Url(salt) : null, hash ? encodeBase64Url(hash) : null, width, height);
      for (let part = 0; part < chunks.length; part++)
        this.sql.exec('INSERT INTO chunks(image_id, part, bytes) VALUES(?, ?, ?)', id, part, chunks[part].buffer);
      this.sql.exec('INSERT INTO usage_daily(day, uploads, views, bytes_uploaded) VALUES(?, 1, 0, ?) ON CONFLICT(day) DO UPDATE SET uploads=uploads+1, bytes_uploaded=bytes_uploaded+excluded.bytes_uploaded',
        dayOf(now), total);
      });
    } catch (failure) {
      if (failure instanceof LimitStop) return failure.response;
      throw failure;
    }
    await this.nextAlarm();
    return json({ id, url: `${origin}/i/${id}` }, 201);
  }
  /**
   * Views are statistics only: a quota never hides an image. Each visitor
   * (salted, daily-rotated hash of the IP) counts once per image per day.
   */
  async recordView(request, record, now) {
    const day = dayOf(now);
    const visitor = await visitorKey(request, this.visitorSalt(), day);
    this.ctx.storage.transactionSync(() => {
      const fresh = this.sql.exec('INSERT OR IGNORE INTO view_marks(day, image_id, visitor) VALUES(?, ?, ?)', day, record.id, visitor).rowsWritten > 0;
      if (fresh) {
        this.sql.exec('INSERT INTO usage_daily(day, uploads, views, bytes_uploaded) VALUES(?, 0, 1, 0) ON CONFLICT(day) DO UPDATE SET views=views+1', day);
        this.sql.exec('UPDATE images SET views=views+1 WHERE id=?', record.id);
      }
    });
  }
  /**
   * The image bytes, inline or as a download; a HEAD gets the same headers without reading
   * storage or counting a view.
   */
  async imageResponse(request, record, now, unlocked, download) {
    const head = request.method === 'HEAD';
    let body = null;
    if (!head) {
      const pieces = this.sql.exec('SELECT bytes FROM chunks WHERE image_id = ? ORDER BY part', record.id).toArray();
      if (!pieces.length || pieces.reduce((length, row) => length + row.bytes.byteLength, 0) !== record.size_bytes)
        throw new Error('Image storage is incomplete.');
      await this.recordView(request, record, now);
      body = new Blob(pieces.map(row => row.bytes), { type: record.content_type });
    }
    // A password-protected image is only served to a visitor holding the cookie from the unlock redirect.
    const cache = unlocked ? { 'Cache-Control': 'private, no-store', Vary: 'Cookie' } : imageCaching(record, now);
    return new Response(body, { headers: {
      ...SHARE_HEADERS, ...cache, 'Content-Type': record.content_type, 'Content-Length': String(record.size_bytes),
      'Content-Security-Policy': "default-src 'none'; sandbox",
      'Content-Disposition': `${download ? 'attachment' : 'inline'}; filename="${shareFilename(record)}"`,
    } });
  }
  findImage(id, now) {
    return this.sql.exec('SELECT * FROM images WHERE id = ? AND expires_at > ?', id, Math.floor(now / 1000)).toArray()[0];
  }
  /**
   * GET and HEAD of /i/<id>[.png|.jpg]: the image, its HTML page or the password form.
   * `?download=1` asks for the bytes as an attachment and `?raw=1` for them inline, whatever the
   * address or the request headers say; both go through the same checks as the page.
   */
  async view(request, name, now) {
    const match = SHARE_NAME.exec(name);
    const record = match && this.findImage(match[1], now);
    if (!record || (match[2] && match[2] !== extensionOf(record))) return viewerError(request, 404, 'gone');
    const protectedShare = Boolean(record.password_hash);
    if (protectedShare && !await this.unlocked(request, record.id, now)) return passwordPage(request, record.id);
    const query = new URL(request.url).searchParams;
    const download = query.get('download') === '1';
    if (!match[2] && !download && query.get('raw') !== '1' && wantsPage(request)) {
      if (request.method !== 'HEAD') await this.recordView(request, record, now);
      return sharePage(request, record, protectedShare);
    }
    if (!protectedShare && request.headers.get('If-None-Match') === etag(record.id)) return notModified(record, now);
    return await this.imageResponse(request, record, now, protectedShare, download);
  }
  /** POST /i/<id>/unlock: a correct password redirects to the image with a signed cookie. */
  async unlock(request, id, now) {
    const record = IMAGE_ID.test(id) && this.findImage(id, now);
    if (!record) return viewerError(request, 404, 'gone');
    if (!record.password_hash)
      return new Response(null, { status: 303, headers: { ...NO_STORE, ...SHARE_HEADERS, Location: `/i/${id}` } });
    // Reserve the attempt before the first await, or concurrent guesses would all see the same count.
    const hour = Math.floor(now / 3_600_000);
    const used = this.sql.exec('INSERT INTO password_attempts(id,hour,failures) VALUES(?,?,1) ON CONFLICT(id,hour) DO UPDATE SET failures=failures+1 RETURNING failures', id, hour).one().failures;
    if (used > PASSWORD_ATTEMPTS_PER_HOUR) {
      const seconds = Math.ceil(((hour + 1) * 3_600_000 - now) / 1000);
      return viewerError(request, 429, 'tooMany', { 'Retry-After': String(seconds) });
    }
    const encoded = await limitedBody(request, 4096);
    let password = null;
    try { password = new URLSearchParams(decoder.decode(encoded)).get('password') || ''; }
    catch { /* not UTF-8: it cannot be the password */ }
    if (password === null || password.length > 128 ||
      !equalBytes(await digestPassword(password, decodeBase64Url(record.password_salt)), decodeBase64Url(record.password_hash)))
      return passwordPage(request, id, true);
    this.sql.exec('UPDATE password_attempts SET failures=failures-1 WHERE id=? AND hour=?', id, hour);
    return new Response(null, { status: 303, headers: {
      ...NO_STORE, ...SHARE_HEADERS, Location: `/i/${id}`, 'Set-Cookie': await this.unlockCookie(id, now),
    } });
  }
  async updateSettings(request, now) {
    const body = await limitedBody(request, 4096);
    let value;
    try { value = JSON.parse(decoder.decode(body)); }
    catch { return error(400, 'Invalid settings JSON.'); }
    if (!value || typeof value !== 'object' || Array.isArray(value)) return error(400, 'Settings must be an object.');
    if (!Object.keys(value).length) return error(400, 'No settings provided.');
    for (const [key, input] of Object.entries(value)) {
      if (key === 'limit_action') {
        if (!['warn', 'block_upload', 'block_all'].includes(input)) return error(400, 'Invalid limit action.');
        // Viewing is never blocked; the former third choice maps to stopping uploads.
        if (input === 'block_all') value[key] = 'block_upload';
      } else if (!Object.hasOwn(SETTINGS, key) || !Number.isSafeInteger(input) || input < SETTINGS[key][0] || input > SETTINGS[key][1]) {
        return error(400, `Invalid ${key}.`);
      }
    }
    const entries = Object.entries(value);
    this.ctx.storage.transactionSync(() => {
      this.sql.exec(`UPDATE preferences SET ${entries.map(([key]) => `${key}=?`).join(', ')} WHERE id=1`, ...entries.map(([, input]) => input));
      const prefs = this.settings();
      this.sql.exec('UPDATE images SET expires_at=created_at+?', prefs.retention_days * 86400);
      this.trim(now, prefs.max_active);
    });
    await this.nextAlarm();
    return json(this.settings());
  }
  stats(now) {
    const settings = this.settings(), daily = this.daily(now), month = monthOf(now);
    const monthly = this.sql.exec('SELECT COALESCE(SUM(uploads),0) uploads, COALESCE(SUM(views),0) views, COALESCE(SUM(bytes_uploaded),0) bytes_uploaded FROM usage_daily WHERE day >= ? AND day < ?', `${month}-01`, `${month}-32`).one();
    const totals = this.sql.exec('SELECT COALESCE(SUM(uploads),0) uploads, COALESCE(SUM(views),0) views FROM usage_daily').one();
    const images = this.stored();
    this.pruneViews(now);
    return json({ worker_version: WORKER_VERSION, daily, monthly, totals, images, settings, estimates: {
      worker_requests: monthly.uploads + monthly.views * 2,
      free_storage_bytes: FREE_STORAGE_BYTES,
      note: 'Free limits apply to the entire Cloudflare account; other Workers and Objects are not included here.',
    }, warning: {
      uploads: daily.uploads >= settings.daily_upload_limit * settings.warning_percent / 100,
      views: daily.views >= settings.daily_view_limit * settings.warning_percent / 100,
      storage: images.stored_bytes >= settings.max_storage_bytes * settings.warning_percent / 100,
    } });
  }
  list(request, origin) {
    const url = new URL(request.url);
    const limit = Number(url.searchParams.get('limit') || 50), offset = Number(url.searchParams.get('offset') || 0);
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100 ||
      !Number.isSafeInteger(offset) || offset < 0 || offset > 100_000)
      return error(400, 'Invalid page.');
    const rows = this.sql.exec('SELECT id, created_at, expires_at, size_bytes, views, password_hash IS NOT NULL AS password_protected FROM images WHERE expires_at > ? ORDER BY created_at DESC, rowid DESC LIMIT ? OFFSET ?', Math.floor(Date.now() / 1000), limit, offset).toArray();
    return json({ images: rows.map(row => ({ ...row, url: `${origin}/i/${row.id}` })), next_offset: rows.length === limit ? offset + limit : null });
  }
  async fetch(request) {
    const now = Date.now(), url = new URL(request.url), path = url.pathname;
    try {
      if (path === '/api/setup' && request.method === 'POST') return json({ status: 'ready', origin: url.origin, version: WORKER_VERSION });
      if (path === '/api/settings') {
        if (request.method === 'GET') return json(this.settings());
        if (request.method === 'PUT') return await this.updateSettings(request, now);
      }
      if (path === '/api/stats' && request.method === 'GET') return this.stats(now);
      if (path === '/api/images' && request.method === 'GET') return this.list(request, url.origin);
      const deletion = /^\/api\/images\/([A-Za-z0-9_-]{32})$/.exec(path);
      if (deletion && request.method === 'DELETE') {
        const exists = this.sql.exec('SELECT id FROM images WHERE id=?', deletion[1]).toArray()[0];
        if (!exists) return error(404, 'Image not found.');
        this.ctx.storage.transactionSync(() => this.remove(deletion[1]));
        await this.nextAlarm();
        return json({ deleted: true });
      }
      if (path === '/api/upload' && request.method === 'POST') return await this.upload(request, now, url.origin);
      const display = /^\/i\/([^/]+)$/.exec(path);
      if (display) {
        if (request.method === 'GET' || request.method === 'HEAD') return await this.view(request, display[1], now);
        return viewerError(request, 405, 'invalid', { Allow: 'GET, HEAD' });
      }
      const unlock = /^\/i\/([^/]+)\/unlock$/.exec(path);
      if (unlock) {
        if (request.method === 'POST') return await this.unlock(request, unlock[1], now);
        return viewerError(request, 405, 'invalid', { Allow: 'POST' });
      }
      return path.startsWith('/i/') ? viewerError(request, 404, 'gone') : error(404, 'Not found.');
    } catch (failure) {
      if (failure instanceof TooLarge) return this.failure(request, 413, failure.message, 'invalid');
      if (failure instanceof RangeError || failure instanceof SyntaxError) return this.failure(request, 400, failure.message, 'invalid');
      if (/SQLITE_FULL|database or disk is full/i.test(String(failure)))
        return this.failure(request, 507, 'Free storage is full. Delete images to make space.', 'unavailable');
      console.error('Share request failed', String(failure));
      return this.failure(request, 503, 'Service temporarily unavailable.', 'unavailable');
    }
  }
  /** JSON for the app's /api/ calls, a page in the viewer's language everywhere else. */
  failure(request, status, message, kind) {
    return new URL(request.url).pathname.startsWith('/api/') ? error(status, message) : viewerError(request, status, kind);
  }
}

export default {
  async fetch(request, env) {
    const path = new URL(request.url).pathname;
    const api = path.startsWith('/api/');
    try { originFor(request, env); }
    catch { return api ? error(421, 'Unknown or insecure hostname.') : viewerError(request, 421, 'invalid'); }
    if (path === '/api/upload' && request.method === 'POST') {
      if (!await authorized(request, env.UPLOAD_TOKEN)) return error(401, 'Upload token required.');
    } else if (path === '/api/setup' || path === '/api/settings' || path === '/api/stats' ||
      path === '/api/images' || path.startsWith('/api/images/')) {
      if (!await authorized(request, env.ADMIN_TOKEN)) return error(401, 'Administrator token required.');
    } else if (api) {
      return error(404, 'Not found.');
    } else {
      const reply = edgeReply(request, path);
      if (reply) return reply;
      // Per-IP rate limit on viewing and unlocking, when the binding exists.
      if (env.VIEW_LIMITER) {
        const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
        const { success } = await env.VIEW_LIMITER.limit({ key: ip });
        if (!success) return viewerError(request, 429, 'busy', { 'Retry-After': '60' });
      }
    }
    try {
      const stub = env.STORE.getByName('installation');
      return await stub.fetch(request);
    } catch (failure) {
      console.error('Store unavailable', String(failure));
      return api ? error(503, 'Service temporarily unavailable.') : viewerError(request, 503, 'unavailable');
    }
  },
};
