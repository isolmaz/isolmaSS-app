import { DurableObject } from 'cloudflare:workers';

/** Reported by /api/setup and /api/stats so the app can offer a Worker update. */
const WORKER_VERSION = 3;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const IMAGE_ID = /^[A-Za-z0-9_-]{32}$/;
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
const NO_STORE = {
  'Cache-Control': 'no-store, max-age=0',
  'Referrer-Policy': 'no-referrer',
  'X-Content-Type-Options': 'nosniff',
};

function json(value, status = 200, extra = {}) {
  return new Response(JSON.stringify(value), {
    status, headers: { ...NO_STORE, 'Content-Type': 'application/json; charset=utf-8', ...extra },
  });
}
/** Public images never change under their random ID, so browsers may keep them. */
const IMMUTABLE = {
  'Cache-Control': 'public, max-age=31536000, immutable',
  'Referrer-Policy': 'no-referrer',
  'X-Content-Type-Options': 'nosniff',
};
function etag(id) { return `"${id}"`; }
function notModified(id) {
  return new Response(null, { status: 304, headers: { ...IMMUTABLE, ETag: etag(id) } });
}
async function visitorKey(request, salt, day) {
  const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
  const digest = await crypto.subtle.digest('SHA-256', encoder.encode(`${salt}|${day}|${ip}`));
  return encodeBase64Url(new Uint8Array(digest).subarray(0, 16));
}
function error(status, message) { return json({ error: message }, status); }
class LimitStop extends Error {
  constructor(response) { super('Upload limit reached.'); this.response = response; }
}
function dayOf(now) { return new Date(now).toISOString().slice(0, 10); }
function monthOf(now) { return dayOf(now).slice(0, 7); }
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
      if (total > maximum) { await reader.cancel(); throw new RangeError('Request body is too large.'); }
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
    throw new RangeError('Image exceeds the configured size limit.');
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
      if (total > maximum) { await reader.cancel(); throw new RangeError('Image exceeds the configured size limit.'); }
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
  if (type === 'image/png') {
    if (!equalBytes(prefix.subarray(0, 8), Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10)) ||
      prefix[12] !== 73 || prefix[13] !== 72 || prefix[14] !== 68 || prefix[15] !== 82)
      throw new RangeError('Invalid PNG image.');
    const view = new DataView(prefix.buffer);
    const width = view.getUint32(16), height = view.getUint32(20);
    if (!width || !height || width > 16384 || height > 16384 || width * height > 100_000_000)
      throw new RangeError('Image dimensions exceed the limit.');
  } else if (prefix[0] !== 255 || prefix[1] !== 216 || prefix[2] !== 255 ||
    secondLastByte !== 255 || lastByte !== 217) {
    throw new RangeError('Invalid JPEG image.');
  }
  if (used) chunks.push(current.slice(0, used));
  return { chunks, total };
}
const PASSWORD_TEXT = {
  en: { title: 'Protected image', heading: 'This image is password protected', note: 'Ask the person who shared it for the password.', label: 'Image password', button: 'Open image' },
  tr: { title: 'Şifreli görüntü', heading: 'Bu görüntü şifreli', note: 'Şifreyi paylaşan kişiden isteyin.', label: 'Görüntü şifresi', button: 'Görüntüyü aç' },
};
/** Turkish for browsers that prefer it, otherwise English. */
function viewerLanguage(request) {
  const first = (request.headers.get('Accept-Language') || '').split(',')[0].trim().toLowerCase();
  return first.startsWith('tr') ? 'tr' : 'en';
}
function passwordPage(id, lang) {
  const text = PASSWORD_TEXT[lang];
  const html = `<!doctype html><html lang="${lang}"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${text.title} · isolmaSS</title><style>body{font:16px system-ui;margin:8vh auto;padding:1rem;max-width:34rem;background:#101821;color:#f4f6f8}form{display:grid;gap:1rem}input,button{font:inherit;padding:.8rem}button{cursor:pointer}</style><main><h1>${text.heading}</h1><p>${text.note}</p><form method="post" action="/i/${id}/unlock"><label for="password">${text.label}</label><input id="password" name="password" type="password" required autocomplete="off"><button>${text.button}</button></form></main></html>`;
  return new Response(html, { headers: {
    ...NO_STORE, 'Content-Type': 'text/html; charset=utf-8',
    'Content-Security-Policy': "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'",
    'X-Frame-Options': 'DENY',
  } });
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
    if (!this.sql.exec("SELECT value FROM meta WHERE key='visitor_salt'").toArray().length)
      this.sql.exec("INSERT INTO meta(key, value) VALUES('visitor_salt', ?)", randomId());
  }

  visitorSalt() { return this.sql.exec("SELECT value FROM meta WHERE key='visitor_salt'").one().value; }
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
    const { chunks, total } = await screenshot(request, settings.max_image_bytes, type);
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
      if (daily.uploads >= 1000 || (prefs.limit_action === 'block_upload' &&
        (daily.uploads >= Math.max(1, Math.floor(prefs.daily_upload_limit * threshold)) ||
          occupied + total > prefs.max_storage_bytes * threshold))) {
        throw new LimitStop(error(429, 'Upload limit reached. Change limits or wait for reset.'));
      }
      if (occupied + total > prefs.max_storage_bytes) {
        throw new LimitStop(error(507, 'Free storage limit reached. Delete images or lower retention.'));
      }
      this.sql.exec('INSERT INTO images(id, content_type, size_bytes, created_at, expires_at, password_salt, password_hash) VALUES(?, ?, ?, ?, ?, ?, ?)',
        id, type, total, nowSeconds, nowSeconds + prefs.retention_days * 86400,
        salt ? encodeBase64Url(salt) : null, hash ? encodeBase64Url(hash) : null);
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
  async imageResponse(request, record, now, unlocked) {
    const pieces = this.sql.exec('SELECT bytes FROM chunks WHERE image_id = ? ORDER BY part', record.id).toArray();
    if (!pieces.length || pieces.reduce((length, row) => length + row.bytes.byteLength, 0) !== record.size_bytes)
      throw new Error('Image storage is incomplete.');
    // Views are statistics only: a quota never hides an image. Each visitor
    // (salted, daily-rotated hash of the IP) counts once per image per day.
    const day = dayOf(now);
    const visitor = await visitorKey(request, this.visitorSalt(), day);
    this.ctx.storage.transactionSync(() => {
      const fresh = this.sql.exec('INSERT OR IGNORE INTO view_marks(day, image_id, visitor) VALUES(?, ?, ?)', day, record.id, visitor).rowsWritten > 0;
      if (fresh) {
        this.sql.exec('INSERT INTO usage_daily(day, uploads, views, bytes_uploaded) VALUES(?, 0, 1, 0) ON CONFLICT(day) DO UPDATE SET views=views+1', day);
        this.sql.exec('UPDATE images SET views=views+1 WHERE id=?', record.id);
      }
    });
    let part = 0;
    const cache = unlocked ? NO_STORE : { ...IMMUTABLE, ETag: etag(record.id) };
    return new Response(new ReadableStream({
      pull(controller) {
        if (part < pieces.length) controller.enqueue(new Uint8Array(pieces[part++].bytes));
        else controller.close();
      },
    }), { headers: {
      ...cache, 'Content-Type': record.content_type,
      'Content-Security-Policy': "default-src 'none'; sandbox",
      'Content-Disposition': `inline; filename="isolmass.${record.content_type === 'image/png' ? 'png' : 'jpg'}"`,
    } });
  }
  async view(request, id, now, unlock) {
    if (!IMAGE_ID.test(id)) return error(404, 'Image not found.');
    const record = this.sql.exec('SELECT * FROM images WHERE id = ? AND expires_at > ?', id, Math.floor(now / 1000)).toArray()[0];
    if (!record) return error(404, 'Image not found.');
    if (!record.password_hash) {
      if (unlock) return error(405, 'Not password protected.');
      if (request.headers.get('If-None-Match') === etag(record.id)) return notModified(record.id);
      return await this.imageResponse(request, record, now, false);
    }
    if (!unlock) return passwordPage(id, viewerLanguage(request));
    const hour = Math.floor(now / 3_600_000);
    const attempt = this.sql.exec('SELECT failures FROM password_attempts WHERE id=? AND hour=?', id, hour).toArray()[0];
    if (attempt?.failures >= 20) return error(429, 'Too many attempts. Try again in one hour.');
    const encoded = await limitedBody(request, 4096);
    let password;
    try { password = new URLSearchParams(decoder.decode(encoded)).get('password') || ''; }
    catch { return error(400, 'Invalid password encoding.'); }
    if (password.length > 128) return error(400, 'Invalid password.');
    const hash = await digestPassword(password, decodeBase64Url(record.password_salt));
    if (!equalBytes(hash, decodeBase64Url(record.password_hash))) {
      this.sql.exec('INSERT INTO password_attempts(id,hour,failures) VALUES(?,?,1) ON CONFLICT(id,hour) DO UPDATE SET failures=failures+1', id, hour);
      return error(401, 'Wrong password.');
    }
    return await this.imageResponse(request, record, now, true);
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
      } else if (!SETTINGS[key] || !Number.isSafeInteger(input) || input < SETTINGS[key][0] || input > SETTINGS[key][1]) {
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
      if (display && request.method === 'GET') return await this.view(request, display[1], now, false);
      const unlock = /^\/i\/([^/]+)\/unlock$/.exec(path);
      if (unlock && request.method === 'POST') return await this.view(request, unlock[1], now, true);
      return error(404, 'Not found.');
    } catch (failure) {
      if (failure instanceof RangeError || failure instanceof SyntaxError) return error(400, failure.message);
      if (/SQLITE_FULL|database or disk is full/i.test(String(failure))) return error(507, 'Free storage is full. Delete images to make space.');
      console.error('Share request failed', String(failure));
      return error(503, 'Service temporarily unavailable.');
    }
  }
}

export default {
  async fetch(request, env) {
    let origin;
    try { origin = originFor(request, env); }
    catch { return error(421, 'Unknown or insecure hostname.'); }
    const path = new URL(request.url).pathname;
    if (path === '/api/upload' && request.method === 'POST') {
      if (!await authorized(request, env.UPLOAD_TOKEN)) return error(401, 'Upload token required.');
    } else if (path === '/api/setup' || path === '/api/settings' || path === '/api/stats' ||
      path === '/api/images' || path.startsWith('/api/images/')) {
      if (!await authorized(request, env.ADMIN_TOKEN)) return error(401, 'Administrator token required.');
    } else if (path.startsWith('/api/')) {
      return error(404, 'Not found.');
    } else {
      // A browser that already holds the (immutable) image revalidates for free.
      const shown = /^\/i\/([A-Za-z0-9_-]{32})$/.exec(path);
      if (shown && request.method === 'GET' && request.headers.get('If-None-Match') === etag(shown[1]))
        return notModified(shown[1]);
      // Per-IP rate limit on viewing and unlocking, when the binding exists.
      if (env.VIEW_LIMITER) {
        const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
        const { success } = await env.VIEW_LIMITER.limit({ key: ip });
        if (!success) return json({ error: 'Too many requests. Try again in a minute.' }, 429, { 'Retry-After': '60' });
      }
    }
    try {
      const stub = env.STORE.getByName('installation');
      return await stub.fetch(request);
    } catch (failure) {
      console.error('Store unavailable', String(failure));
      return error(503, 'Service temporarily unavailable.');
    }
  },
};
