import { evictDurableObject, reset, runDurableObjectAlarm, runInDurableObject } from 'cloudflare:test';
import { beforeEach, describe, expect, it } from 'vitest';
import {
  ORIGIN, PASSWORD, admin, adminAuth, call, jpeg, png, putSettings, sql, store, unlock, upload, uploadAuth,
} from './helpers.mjs';

const JSON_TYPE = 'application/json; charset=utf-8';
const DAY = 86400;

beforeEach(async () => { await reset(); });

const alarmAt = () => runInDurableObject(store(), (_instance, state) => state.storage.getAlarm());
const nowSeconds = () => Math.floor(Date.now() / 1000);
const settingsPut = body => admin('/api/settings', { method: 'PUT', body });

async function expectError(response, status, message) {
  expect(response.status).toBe(status);
  expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
  expect(await response.json()).toEqual({ error: message });
}

describe('administrator and upload tokens', () => {
  it('open only their own endpoints', async () => {
    await expectError(await call('/api/stats', { headers: uploadAuth() }), 401, 'Administrator token required.');
    const body = png();
    const response = await call('/api/upload', { method: 'POST', headers: { ...adminAuth(), 'Content-Type': 'image/png' }, body });
    await expectError(response, 401, 'Upload token required.');
    expect(await sql('SELECT id FROM images')).toEqual([]);
  });

  it.each([
    ['no header', {}],
    ['a different scheme', { Authorization: 'Basic ' + 'a'.repeat(43) }],
    ['a token that is too short', { Authorization: 'Bearer ' + 'a'.repeat(42) }],
    ['a token that is too long', { Authorization: 'Bearer ' + 'a'.repeat(44) }],
    ['characters outside base64url', { Authorization: 'Bearer ' + '+'.repeat(43) }],
    ['a well-formed wrong token', { Authorization: 'Bearer ' + 'b'.repeat(43) }],
  ])('are refused with %s', async (_name, headers) => {
    await expectError(await call('/api/stats', { headers }), 401, 'Administrator token required.');
  });

  it('are refused for everyone when the Worker has no secret configured', async () => {
    const response = await call('/api/stats', { headers: adminAuth() }, { ADMIN_TOKEN: '' });
    await expectError(response, 401, 'Administrator token required.');
  });
});

describe('setup', () => {
  it('tells the app the Worker is ready, with its address and version', async () => {
    const response = await admin('/api/setup', { method: 'POST' });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    // The app compares this number with the one it ships to offer a Worker update.
    expect(await response.json()).toEqual({ status: 'ready', origin: ORIGIN, version: 4 });
  });

  it('reports the same version as the statistics', async () => {
    const setup = await (await admin('/api/setup', { method: 'POST' })).json();
    const stats = await (await admin('/api/stats')).json();
    expect(setup.version).toBe(stats.worker_version);
  });
});

describe('endpoints with the wrong method or path', () => {
  it.each([
    ['GET', '/api/setup'],
    ['POST', '/api/settings'],
    ['DELETE', '/api/settings'],
    ['POST', '/api/stats'],
    ['POST', '/api/images'],
    ['GET', `/api/images/${'A'.repeat(32)}`],
    ['DELETE', '/api/images/short'],
    ['DELETE', `/api/images/${'A'.repeat(33)}`],
  ])('answer %s %s with a JSON 404', async (method, path) => {
    await expectError(await admin(path, { method }), 404, 'Not found.');
  });

  it.each([
    ['GET', '/api/upload'],
    ['GET', '/api/nothing'],
    ['PUT', '/api/upload'],
  ])('answer %s %s with a JSON 404 even without a token', async (method, path) => {
    await expectError(await call(path, { method }), 404, 'Not found.');
  });
});

describe('listing images', () => {
  const list = query => admin(`/api/images${query}`);

  it('describes each image, newest first', async () => {
    const bytes = png({ size: 300 });
    const first = await upload(bytes);
    const second = await upload(jpeg(120), { type: 'image/jpeg', password: PASSWORD });
    const response = await list('');
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
    const { images, next_offset: next } = await response.json();
    expect(next).toBeNull();
    expect(images.map(image => image.id)).toEqual([second, first]);
    const [newest, oldest] = images;
    expect(oldest).toEqual({
      id: first, created_at: oldest.created_at, expires_at: oldest.created_at + 30 * DAY, size_bytes: 300,
      views: 0, password_protected: 0, url: `${ORIGIN}/i/${first}`,
    });
    expect(Math.abs(oldest.created_at - nowSeconds())).toBeLessThanOrEqual(5);
    expect(newest).toMatchObject({ id: second, size_bytes: 120, password_protected: 1, url: `${ORIGIN}/i/${second}` });
    expect(newest).not.toHaveProperty('password_hash');
    expect(newest).not.toHaveProperty('password_salt');
  });

  it('counts the views of each image', async () => {
    const id = await upload(png());
    await call(`/i/${id}.png`);
    await call(`/i/${id}.png`, { headers: { 'CF-Connecting-IP': '203.0.113.9' } });
    expect((await (await list('')).json()).images[0].views).toBe(2);
  });

  it('is empty without images', async () => {
    expect(await (await list('')).json()).toEqual({ images: [], next_offset: null });
  });

  it('pages through the images with next_offset', async () => {
    const ids = [];
    for (let index = 0; index < 5; index++) ids.push(await upload(png()));
    const newestFirst = ids.toReversed();
    const first = await (await list('?limit=2')).json();
    expect(first.images.map(image => image.id)).toEqual(newestFirst.slice(0, 2));
    expect(first.next_offset).toBe(2);
    const second = await (await list(`?limit=2&offset=${first.next_offset}`)).json();
    expect(second.images.map(image => image.id)).toEqual(newestFirst.slice(2, 4));
    expect(second.next_offset).toBe(4);
    const last = await (await list(`?limit=2&offset=${second.next_offset}`)).json();
    expect(last.images.map(image => image.id)).toEqual(newestFirst.slice(4));
    expect(last.next_offset).toBeNull();
  });

  it('points past a full last page, and the page after it is empty', async () => {
    for (let index = 0; index < 3; index++) await upload(png());
    const full = await (await list('?limit=3')).json();
    expect(full.images).toHaveLength(3);
    expect(full.next_offset).toBe(3);
    expect(await (await list('?limit=3&offset=3')).json()).toEqual({ images: [], next_offset: null });
  });

  it('skips images that have expired but are not deleted yet', async () => {
    const live = await upload(png());
    const expired = await upload(png());
    await sql('UPDATE images SET expires_at=? WHERE id=?', nowSeconds() - 1, expired);
    expect((await (await list('')).json()).images.map(image => image.id)).toEqual([live]);
  });

  it.each([
    '?limit=100&offset=100000',
    '?limit=1',
    '?limit=&offset=',
  ])('accepts %s', async query => {
    expect((await list(query)).status).toBe(200);
  });

  it.each([
    '?limit=0', '?limit=101', '?limit=-1', '?limit=1.5', '?limit=abc', '?limit=1e3',
    '?offset=-1', '?offset=100001', '?offset=1.5', '?offset=abc',
  ])('refuses %s with 400', async query => {
    await expectError(await list(query), 400, 'Invalid page.');
  });

  it('needs the administrator token', async () => {
    await expectError(await call('/api/images'), 401, 'Administrator token required.');
  });
});

describe('deleting images', () => {
  it('removes the image with its chunks and attempt counters', async () => {
    const id = await upload(png({ size: 1_200_000 }), { password: PASSWORD });
    const other = await upload(png());
    await unlock(id, 'definitely not it');
    expect((await sql('SELECT failures FROM password_attempts WHERE id=?', id))).toHaveLength(1);
    const response = await admin(`/api/images/${id}`, { method: 'DELETE' });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
    expect(await response.json()).toEqual({ deleted: true });
    expect(await sql('SELECT id FROM images')).toEqual([{ id: other }]);
    expect((await sql('SELECT DISTINCT image_id FROM chunks')).map(row => row.image_id)).toEqual([other]);
    expect(await sql('SELECT id FROM password_attempts')).toEqual([]);
    expect((await call(`/i/${id}`)).status).toBe(404);
    const [{ expires_at }] = await sql('SELECT expires_at FROM images');
    expect(await alarmAt()).toBe(expires_at * 1000);
  });

  it('clears the alarm when the last image is deleted', async () => {
    const id = await upload(png());
    expect(await alarmAt()).not.toBeNull();
    expect((await admin(`/api/images/${id}`, { method: 'DELETE' })).status).toBe(200);
    expect(await alarmAt()).toBeNull();
  });

  it('answers 404 for an image that does not exist, and for one deleted already', async () => {
    const id = await upload(png());
    await expectError(await admin(`/api/images/${'M'.repeat(32)}`, { method: 'DELETE' }), 404, 'Image not found.');
    expect((await admin(`/api/images/${id}`, { method: 'DELETE' })).status).toBe(200);
    await expectError(await admin(`/api/images/${id}`, { method: 'DELETE' }), 404, 'Image not found.');
  });

  it('also deletes an image that expired but was not cleaned up yet', async () => {
    const id = await upload(png());
    await sql('UPDATE images SET expires_at=? WHERE id=?', nowSeconds() - 1, id);
    expect((await admin(`/api/images/${id}`, { method: 'DELETE' })).status).toBe(200);
    expect(await sql('SELECT image_id FROM chunks')).toEqual([]);
  });
});

describe('changing settings', () => {
  it('refuses a missing body', async () => {
    await expectError(await admin('/api/settings', { method: 'PUT' }), 400, 'Missing request body.');
  });

  it.each([
    ['text that is not JSON', '{nope'],
    ['an empty body', ''],
    ['bytes that are not UTF-8', Uint8Array.of(123, 255, 125)],
  ])('refuses %s', async (_name, body) => {
    await expectError(await settingsPut(body), 400, 'Invalid settings JSON.');
  });

  it.each([
    ['null', 'null'],
    ['a number', '5'],
    ['a string', '"retention_days"'],
    ['a boolean', 'true'],
    ['an array', '[{"retention_days":5}]'],
  ])('refuses %s instead of an object', async (_name, body) => {
    await expectError(await settingsPut(body), 400, 'Settings must be an object.');
  });

  it('refuses an object without settings', async () => {
    await expectError(await settingsPut('{}'), 400, 'No settings provided.');
  });

  it('refuses an unknown setting', async () => {
    await expectError(await settingsPut('{"theme":1}'), 400, 'Invalid theme.');
  });

  it.each([
    ['text', '"nope"'],
    ['a number', '5'],
    ['null', 'null'],
    ['the wrong case', '"WARN"'],
  ])('refuses %s as the limit action', async (_name, json) => {
    await expectError(await settingsPut(`{"limit_action":${json}}`), 400, 'Invalid limit action.');
  });

  it.each(['warn', 'block_upload'])('stores the limit action %s', async action => {
    await putSettings({ limit_action: 'warn' });
    const response = await settingsPut(JSON.stringify({ limit_action: action }));
    expect(response.status).toBe(200);
    expect((await response.json()).limit_action).toBe(action);
    expect((await (await admin('/api/settings')).json()).limit_action).toBe(action);
  });

  it('stores the former third limit action as stopping uploads', async () => {
    const response = await settingsPut('{"limit_action":"block_all"}');
    expect(response.status).toBe(200);
    expect((await response.json()).limit_action).toBe('block_upload');
    expect((await (await admin('/api/settings')).json()).limit_action).toBe('block_upload');
  });

  it('turns the former third limit action of an older install into stopping uploads', async () => {
    await sql("UPDATE preferences SET limit_action='block_all'");
    await evictDurableObject(store());
    const settings = await (await admin('/api/settings')).json();
    expect(settings.limit_action).toBe('block_upload');
    expect(await sql('SELECT limit_action FROM preferences')).toEqual([{ limit_action: 'block_upload' }]);
  });

  it.each([
    ['max_active', 1, 5000],
    ['max_image_bytes', 1024, 10 * 1024 * 1024],
    ['max_storage_bytes', 1_048_576, 800_000_000],
    ['daily_upload_limit', 1, 1000],
    ['daily_view_limit', 1, 20_000],
    ['retention_days', 1, 3650],
    ['warning_percent', 1, 100],
  ])('keeps %s between %i and %i', async (key, low, high) => {
    for (const value of [low - 1, high + 1]) {
      await expectError(await settingsPut(JSON.stringify({ [key]: value })), 400, `Invalid ${key}.`);
    }
    for (const value of [low, high]) {
      const response = await settingsPut(JSON.stringify({ [key]: value }));
      expect(response.status).toBe(200);
      expect((await response.json())[key]).toBe(value);
    }
  });

  it.each([['a fraction', '1.5'], ['a string', '"5"'], ['null', 'null'], ['a boolean', 'true'], ['an unsafe integer', '9007199254740993'], ['infinity', '1e999']])(
    'refuses %s as a number', async (_name, value) => {
      await expectError(await settingsPut(`{"retention_days":${value}}`), 400, 'Invalid retention_days.');
    });

  it('applies all of the settings or none', async () => {
    await expectError(await settingsPut('{"retention_days":7,"max_active":0}'), 400, 'Invalid max_active.');
    await expectError(await settingsPut('{"retention_days":7,"limit_action":"nope"}'), 400, 'Invalid limit action.');
    const settings = await (await admin('/api/settings')).json();
    expect(settings).toMatchObject({ retention_days: 30, max_active: 50, limit_action: 'warn' });
  });

  it('changes several settings at once and returns them all', async () => {
    const response = await settingsPut('{"retention_days":7,"daily_upload_limit":5,"limit_action":"block_upload"}');
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      id: 1, max_active: 50, max_image_bytes: 10_485_760, max_storage_bytes: 800_000_000, daily_upload_limit: 5,
      daily_view_limit: 1000, retention_days: 7, warning_percent: 90, limit_action: 'block_upload',
    });
  });

  it('moves the expiry of existing images when the retention changes', async () => {
    const id = await upload(png());
    await putSettings({ retention_days: 3 });
    const [{ created_at, expires_at }] = await sql('SELECT created_at, expires_at FROM images WHERE id=?', id);
    expect(expires_at).toBe(created_at + 3 * DAY);
    expect(await alarmAt()).toBe(expires_at * 1000);
    expect((await (await admin('/api/images')).json()).images[0].expires_at).toBe(expires_at);
  });

  it('deletes the oldest images when max_active drops below their number', async () => {
    const first = await upload(png());
    const second = await upload(png());
    const third = await upload(png());
    await putSettings({ max_active: 2 });
    expect((await sql('SELECT id FROM images ORDER BY rowid')).map(row => row.id)).toEqual([second, third]);
    expect((await sql('SELECT DISTINCT image_id FROM chunks')).map(row => row.image_id).sort()).toEqual([second, third].sort());
    expect((await call(`/i/${first}`)).status).toBe(404);
    expect(await alarmAt()).not.toBeNull();
  });
});

describe('expiry alarm', () => {
  it('is set for the earliest expiry of all images', async () => {
    await upload(png());
    await putSettings({ retention_days: 5 });
    await upload(png());
    const [{ soonest }] = await sql('SELECT MIN(expires_at) AS soonest FROM images');
    expect(await alarmAt()).toBe(soonest * 1000);
  });

  it('deletes expired images with their chunks and attempt counters, then waits for the next expiry', async () => {
    const keep = await upload(png());
    const gone = await upload(png({ size: 1_200_000 }), { password: PASSWORD });
    await unlock(gone, 'definitely not it');
    await sql('UPDATE images SET expires_at=? WHERE id=?', nowSeconds() - 5, gone);
    expect(await runDurableObjectAlarm(store())).toBe(true);
    expect((await sql('SELECT id FROM images')).map(row => row.id)).toEqual([keep]);
    expect((await sql('SELECT DISTINCT image_id FROM chunks')).map(row => row.image_id)).toEqual([keep]);
    expect(await sql('SELECT id FROM password_attempts')).toEqual([]);
    const [{ expires_at }] = await sql('SELECT expires_at FROM images');
    expect(await alarmAt()).toBe(expires_at * 1000);
  });

  it('stops when the last image is gone', async () => {
    const id = await upload(png());
    await sql('UPDATE images SET expires_at=? WHERE id=?', nowSeconds() - 5, id);
    expect(await runDurableObjectAlarm(store())).toBe(true);
    expect(await sql('SELECT id FROM images')).toEqual([]);
    expect(await sql('SELECT image_id FROM chunks')).toEqual([]);
    expect(await alarmAt()).toBeNull();
  });

  it('keeps only max_active images', async () => {
    await upload(png());
    const second = await upload(png());
    const third = await upload(png());
    await sql('UPDATE preferences SET max_active=2');
    expect(await runDurableObjectAlarm(store())).toBe(true);
    expect((await sql('SELECT id FROM images ORDER BY rowid')).map(row => row.id)).toEqual([second, third]);
    expect(await sql('SELECT DISTINCT image_id FROM chunks ORDER BY image_id')).toHaveLength(2);
  });

  it('forgets view marks of earlier days and attempt counters older than a day', async () => {
    const id = await upload(png());
    const today = new Date().toISOString().slice(0, 10);
    const hour = Math.floor(Date.now() / 3_600_000);
    await sql(`INSERT INTO view_marks(day, image_id, visitor) VALUES ('2020-01-01', ?, 'old'), (?, ?, 'today')`, id, today, id);
    await sql(`INSERT INTO password_attempts(id, hour, failures) VALUES ('stale', ?, 3), ('recent', ?, 4)`, hour - 25, hour - 23);
    expect(await runDurableObjectAlarm(store())).toBe(true);
    expect((await sql('SELECT visitor FROM view_marks'))).toEqual([{ visitor: 'today' }]);
    expect(await sql('SELECT id, failures FROM password_attempts')).toEqual([{ id: 'recent', failures: 4 }]);
  });
});

describe('PUBLIC_ORIGIN', () => {
  it.each([
    ['the origin', ORIGIN],
    ['the origin with a path', `${ORIGIN}/ignored/path?x=1`],
    ['the origin in another letter case', 'https://SHARE.test'],
  ])('lets requests for %s through', async (_name, configured) => {
    const response = await call('/api/setup', { method: 'POST', headers: adminAuth() }, { PUBLIC_ORIGIN: configured });
    expect(response.status).toBe(200);
    expect((await response.json()).origin).toBe(ORIGIN);
  });

  it.each([
    ['another hostname', 'https://other.test'],
    ['another port', 'https://share.test:8443'],
    ['another subdomain', 'https://www.share.test'],
  ])('answers 421 for %s', async (_name, configured) => {
    const bindings = { PUBLIC_ORIGIN: configured };
    await expectError(await call('/api/stats', { headers: adminAuth() }, bindings), 421, 'Unknown or insecure hostname.');
    const page = await call(`/i/${'P'.repeat(32)}`, {}, bindings);
    expect(page.status).toBe(421);
    expect(page.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(await page.text()).toContain('Request not supported');
  });

  it('is not needed for the Worker to serve', async () => {
    expect((await call('/api/stats', { headers: adminAuth() }, { PUBLIC_ORIGIN: '' })).status).toBe(200);
  });
});

describe('plain HTTP', () => {
  it.each(['localhost:8787', '127.0.0.1:8787'])('is accepted for local development at %s', async host => {
    const url = `http://${host}/api/upload`;
    const response = await call(url, { method: 'POST', headers: { ...uploadAuth(), 'Content-Type': 'image/png' }, body: png() });
    expect(response.status).toBe(201);
    const { id, url: share } = await response.json();
    expect(share).toBe(`http://${host}/i/${id}`);
  });

  it.each(['192.168.1.5', 'share.test', 'localhost.evil.test'])('is refused for %s', async host => {
    const response = await call(`http://${host}/api/stats`, { headers: adminAuth() });
    await expectError(response, 421, 'Unknown or insecure hostname.');
  });
});
