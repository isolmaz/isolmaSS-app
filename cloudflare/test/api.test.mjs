import { evictDurableObject, reset, runInDurableObject } from 'cloudflare:test';
import { beforeEach, describe, expect, it } from 'vitest';
import {
  admin, adminAuth, call, jpeg, navigate, png, postImage, putSettings, sql, store, streamOf, upload,
} from './helpers.mjs';

const JSON_TYPE = 'application/json; charset=utf-8';
const LIMIT_MESSAGE = 'Upload limit reached. Change limits or wait for reset.';

beforeEach(async () => { await reset(); });

describe('upload', () => {
  it('stores PNG dimensions for the viewer page and none for JPEG', async () => {
    const pngId = await upload(png({ width: 1280, height: 720 }));
    const jpegId = await upload(jpeg(), { type: 'image/jpeg' });
    const [first] = await sql('SELECT width, height FROM images WHERE id=?', pngId);
    const [second] = await sql('SELECT width, height FROM images WHERE id=?', jpegId);
    expect(first).toEqual({ width: 1280, height: 720 });
    expect(second).toEqual({ width: null, height: null });
  });

  it('answers 201 with the share address', async () => {
    const response = await postImage(png());
    expect(response.status).toBe(201);
    const { id, url } = await response.json();
    expect(id).toMatch(/^[A-Za-z0-9_-]{32}$/);
    expect(url).toBe(`https://share.test/i/${id}`);
  });

  it('keeps 400 for malformed images', async () => {
    const response = await postImage(new Uint8Array(64).fill(9));
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ error: 'Invalid PNG image.' });
  });

  describe('size limit', () => {
    beforeEach(async () => { await putSettings({ max_image_bytes: 1024 }); });

    it('answers 413 for a body of known length', async () => {
      const response = await postImage(png({ size: 4000 }));
      expect(response.status).toBe(413);
      expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
      expect(await response.json()).toEqual({ error: 'Image exceeds the configured size limit.' });
    });

    it('answers 413 for a streamed body of unknown length', async () => {
      const response = await postImage(streamOf(png({ size: 4000 })));
      expect(response.status).toBe(413);
      expect(await response.json()).toEqual({ error: 'Image exceeds the configured size limit.' });
    });

    it('accepts an image at the limit', async () => {
      expect((await postImage(png({ size: 1024 }))).status).toBe(201);
    });
  });

  it('answers 413 for an oversized settings body', async () => {
    const response = await admin('/api/settings', { method: 'PUT', body: JSON.stringify({ retention_days: 5, padding: 'x'.repeat(5000) }) });
    expect(response.status).toBe(413);
    expect(await response.json()).toEqual({ error: 'Request body is too large.' });
  });
});

describe('daily upload limit', () => {
  beforeEach(async () => {
    await putSettings({ daily_upload_limit: 1, limit_action: 'block_upload', warning_percent: 100 });
  });

  it('answers 429 before reading the image, with the seconds until the UTC reset', async () => {
    await upload(png());
    // Not an image: reading it would give 400, so a 429 proves the quota was checked first.
    const response = await postImage(new Uint8Array(50).fill(9));
    expect(response.status).toBe(429);
    expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
    expect(await response.json()).toEqual({ error: LIMIT_MESSAGE });
    const wait = Number(response.headers.get('Retry-After'));
    const untilMidnight = 86400 - (Date.now() / 1000) % 86400;
    expect(Number.isInteger(wait)).toBe(true);
    expect(Math.abs(wait - untilMidnight)).toBeLessThanOrEqual(5);
  });

  it('lets only one of two simultaneous uploads through', async () => {
    const responses = await Promise.all([postImage(png()), postImage(png())]);
    expect(responses.map(response => response.status).sort()).toEqual([201, 429]);
    const limited = responses.find(response => response.status === 429);
    expect(await limited.json()).toEqual({ error: LIMIT_MESSAGE });
    expect(Number(limited.headers.get('Retry-After'))).toBeGreaterThan(0);
  });

  it('is not an upload limit when only the warning is on', async () => {
    await putSettings({ limit_action: 'warn' });
    await upload(png());
    expect((await postImage(png())).status).toBe(201);
  });
});

describe('storage limits', () => {
  it('answers 429 without Retry-After when stopping uploads at the storage threshold', async () => {
    await putSettings({ max_storage_bytes: 1_048_576, warning_percent: 1, limit_action: 'block_upload' });
    const response = await postImage(png({ size: 20_000 }));
    expect(response.status).toBe(429);
    expect(await response.json()).toEqual({ error: LIMIT_MESSAGE });
    expect(response.headers.get('Retry-After')).toBeNull();
  });

  it('answers 507 when the image does not fit at all', async () => {
    await putSettings({ max_storage_bytes: 1_048_576 });
    const response = await postImage(png({ size: 1_100_000 }));
    expect(response.status).toBe(507);
    expect(await response.json()).toEqual({ error: 'Free storage limit reached. Delete images or lower retention.' });
  });
});

describe('settings', () => {
  it.each(['toString', 'constructor', '__proto__', 'hasOwnProperty', 'valueOf'])(
    'rejects the inherited name %s with 400, not a database error', async key => {
      const response = await admin('/api/settings', { method: 'PUT', body: `{"${key}":5}` });
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({ error: `Invalid ${key}.` });
    });

  it('applies a valid change and rejects an out-of-range value', async () => {
    const changed = await admin('/api/settings', { method: 'PUT', body: JSON.stringify({ retention_days: 7 }) });
    expect(changed.status).toBe(200);
    expect((await changed.json()).retention_days).toBe(7);
    const rejected = await admin('/api/settings', { method: 'PUT', body: JSON.stringify({ retention_days: 0 }) });
    expect(rejected.status).toBe(400);
    expect(await rejected.json()).toEqual({ error: 'Invalid retention_days.' });
    expect((await (await admin('/api/settings')).json()).retention_days).toBe(7);
  });
});

describe('JSON responses', () => {
  it.each([
    ['an admin reply', () => call('/api/stats', { headers: adminAuth() }), 200],
    ['a missing token', () => call('/api/stats'), 401],
    ['an unknown API path', () => call('/api/nothing'), 404],
    ['an upload without a token', () => call('/api/upload', { method: 'POST' }), 401],
  ])('carry a restrictive CSP and are never stored: %s', async (_name, send, status) => {
    const response = await send();
    expect(response.status).toBe(status);
    expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
    expect(response.headers.get('Content-Security-Policy')).toBe("default-src 'none'; frame-ancestors 'none'");
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(response.headers.get('X-Content-Type-Options')).toBe('nosniff');
    expect(response.headers.get('Referrer-Policy')).toBe('no-referrer');
  });
});

describe('storage schema', () => {
  it('adds the size columns to the images table of an older Worker, once', async () => {
    const now = Math.floor(Date.now() / 1000);
    const legacy = 'L'.repeat(32);
    await runInDurableObject(store(), instance => {
      instance.sql.exec(`DROP TABLE images;
        CREATE TABLE images (id TEXT PRIMARY KEY, content_type TEXT NOT NULL, size_bytes INTEGER NOT NULL,
          created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, password_salt TEXT, password_hash TEXT,
          views INTEGER NOT NULL DEFAULT 0)`);
      instance.sql.exec('INSERT INTO images(id, content_type, size_bytes, created_at, expires_at) VALUES(?, ?, ?, ?, ?)',
        legacy, 'image/png', 64, now, now + 1000);
      expect(instance.sql.exec('PRAGMA table_info(images)').toArray().map(column => column.name)).not.toContain('width');
    });
    for (let restart = 0; restart < 2; restart++) {
      await evictDurableObject(store());
      // evictDurableObject waits for in-flight requests, and a response body
      // nobody reads keeps its request in flight, so the next eviction would hang.
      const stats = await admin('/api/stats');
      expect(stats.status).toBe(200);
      await stats.text();
      const names = (await sql('PRAGMA table_info(images)')).map(column => column.name);
      expect(names).toEqual(expect.arrayContaining(['width', 'height']));
      expect(names.filter(name => name === 'width')).toHaveLength(1);
    }
    expect((await sql('SELECT width, height FROM images WHERE id=?', legacy))[0]).toEqual({ width: null, height: null });
    const page = await navigate(`/i/${legacy}`);
    expect(page.status).toBe(200);
    expect(await page.text()).not.toContain('og:image:width');
  });

  it('keeps the visitor salt and the cookie key across restarts', async () => {
    const before = await sql('SELECT key, value FROM meta ORDER BY key');
    await evictDurableObject(store());
    expect(await sql('SELECT key, value FROM meta ORDER BY key')).toEqual(before);
    expect(before.map(row => row.key)).toEqual(['unlock_key', 'visitor_salt']);
    expect(before.every(row => row.value.length >= 32)).toBe(true);
  });
});
