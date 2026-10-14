import { reset, runInDurableObject } from 'cloudflare:test';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  ORIGIN, base64Url, bytesOf, call, expectSameBytes, jpeg, navigate, png, postImage, putSettings, sql, store, streamOf, upload, uploadAuth,
} from './helpers.mjs';

const JSON_TYPE = 'application/json; charset=utf-8';
const LIMIT_MESSAGE = 'Upload limit reached. Change limits or wait for reset.';
const PASSWORD_RULE = 'Image password must contain 12–128 printable characters.';

beforeEach(async () => { await reset(); });

const encode = text => base64Url(new TextEncoder().encode(text));

/** A refused upload: the JSON error, and nothing left behind in storage. */
async function expectRejected(response, status, message) {
  expect(response.status).toBe(status);
  expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
  expect(await response.json()).toEqual({ error: message });
  expect(await sql('SELECT id FROM images')).toEqual([]);
  expect(await sql('SELECT image_id FROM chunks')).toEqual([]);
}

describe('content type', () => {
  it.each([
    ['text/plain'],
    ['image/gif'],
    ['image/PNG'],
    ['image/png; charset=binary'],
    [''],
  ])('refuses %j with 400', async type => {
    await expectRejected(await postImage(png(), { 'Content-Type': type }), 400, 'Only PNG and JPEG are accepted.');
  });

  it('refuses an upload without any content type', async () => {
    const response = await call('/api/upload', { method: 'POST', headers: uploadAuth(), body: png() });
    await expectRejected(response, 400, 'Only PNG and JPEG are accepted.');
  });
});

describe('request body', () => {
  it('refuses an upload without a body', async () => {
    await expectRejected(await postImage(undefined), 400, 'No screenshot was sent.');
  });

  it('refuses an image shorter than a PNG header', async () => {
    await expectRejected(await postImage(png().slice(0, 23)), 400, 'Incomplete image.');
    await expectRejected(await postImage(streamOf(jpeg(23), 5), { 'Content-Type': 'image/jpeg' }), 400, 'Incomplete image.');
  });

  it('accepts the shortest image that carries a PNG header', async () => {
    expect((await postImage(png({ size: 24 }))).status).toBe(201);
  });
});

describe('PNG validation', () => {
  it('requires the PNG signature', async () => {
    const bytes = png();
    bytes[1] = 0x51;
    await expectRejected(await postImage(bytes), 400, 'Invalid PNG image.');
  });

  it.each([12, 13, 14, 15])('requires the IHDR chunk name (byte %i)', async index => {
    const bytes = png();
    bytes[index] = 0;
    await expectRejected(await postImage(bytes), 400, 'Invalid PNG image.');
  });

  it('does not take a JPEG for a PNG', async () => {
    await expectRejected(await postImage(jpeg()), 400, 'Invalid PNG image.');
  });

  it.each([
    ['a zero width', { width: 0, height: 480 }],
    ['a zero height', { width: 640, height: 0 }],
    ['a width over 16384', { width: 16385, height: 100 }],
    ['a height over 16384', { width: 100, height: 16385 }],
    ['over 100 million pixels', { width: 12000, height: 12000 }],
    ['just over 100 million pixels', { width: 16384, height: 6104 }],
  ])('refuses %s', async (_name, size) => {
    await expectRejected(await postImage(png(size)), 400, 'Image dimensions exceed the limit.');
  });

  it.each([
    ['the largest side', { width: 16384, height: 1 }],
    ['exactly 100 million pixels', { width: 10000, height: 10000 }],
    ['just under 100 million pixels', { width: 16384, height: 6103 }],
  ])('accepts %s', async (_name, size) => {
    const id = await upload(png(size));
    expect(await sql('SELECT width, height FROM images WHERE id=?', id)).toEqual([size]);
  });
});

describe('JPEG validation', () => {
  const asJpeg = bytes => postImage(bytes, { 'Content-Type': 'image/jpeg' });

  it('accepts a JPEG that starts with FF D8 FF and ends with FF D9', async () => {
    expect((await asJpeg(jpeg())).status).toBe(201);
  });

  it.each([
    ['the first byte', 0],
    ['the second byte', 1],
    ['the third byte', 2],
    ['the second to last byte', 62],
    ['the last byte', 63],
  ])('refuses a JPEG with a wrong %s', async (_name, index) => {
    const bytes = jpeg();
    bytes[index] = 0;
    await expectRejected(await asJpeg(bytes), 400, 'Invalid JPEG image.');
  });

  it('does not take a PNG for a JPEG', async () => {
    await expectRejected(await asJpeg(png()), 400, 'Invalid JPEG image.');
  });

  it('finds the end marker when the body arrives one byte at a time', async () => {
    const response = await asJpeg(streamOf(jpeg(), 1));
    expect(response.status).toBe(201);
    const { id } = await response.json();
    expect(await bytesOf(await call(`/i/${id}.jpg`))).toEqual(jpeg());
  });

  it('finds the end marker when its two bytes arrive in different chunks', async () => {
    expect((await asJpeg(streamOf(jpeg(), 63))).status).toBe(201);
  });

  it('refuses a wrong end marker that arrives one byte at a time', async () => {
    const bytes = jpeg();
    bytes[62] = 7;
    await expectRejected(await asJpeg(streamOf(bytes, 1)), 400, 'Invalid JPEG image.');
  });

  it('skips empty chunks when looking for the end marker', async () => {
    // The runtime never hands the Durable Object an empty chunk, so this gives it the request directly.
    const bytes = jpeg();
    const parts = [bytes.slice(0, 40), new Uint8Array(0), bytes.slice(40, 63), new Uint8Array(0), bytes.slice(63), new Uint8Array(0)];
    const stream = new ReadableStream({
      pull(controller) {
        if (parts.length) controller.enqueue(parts.shift());
        else controller.close();
      },
    });
    const { status, id } = await runInDurableObject(store(), async instance => {
      const request = new Request(`${ORIGIN}/api/upload`, { method: 'POST', headers: { 'Content-Type': 'image/jpeg' }, body: stream });
      const response = await instance.fetch(request);
      return { status: response.status, ...(await response.json()) };
    });
    expect(status).toBe(201);
    expect(await bytesOf(await call(`/i/${id}.jpg`))).toEqual(bytes);
  });
});

describe('storage in chunks', () => {
  it('keeps an image of exactly one chunk as one chunk', async () => {
    const bytes = png({ size: 1_000_000 });
    const id = await upload(bytes);
    expect(await sql('SELECT part, length(bytes) AS size FROM chunks WHERE image_id=? ORDER BY part', id))
      .toEqual([{ part: 0, size: 1_000_000 }]);
    const response = await call(`/i/${id}.png`);
    expect(response.headers.get('Content-Length')).toBe('1000000');
    expectSameBytes(await bytesOf(response), bytes);
  });

  it('puts the one byte over a chunk into a second chunk', async () => {
    const id = await upload(png({ size: 1_000_001 }));
    expect(await sql('SELECT part, length(bytes) AS size FROM chunks WHERE image_id=? ORDER BY part', id))
      .toEqual([{ part: 0, size: 1_000_000 }, { part: 1, size: 1 }]);
  });

  it('reassembles an image streamed in pieces that do not line up with the chunks', async () => {
    const bytes = png({ size: 2_300_000 });
    for (let index = 24; index < bytes.length; index += 1) bytes[index] = index % 241;
    const response = await postImage(streamOf(bytes, 700_001));
    expect(response.status).toBe(201);
    const { id } = await response.json();
    expect((await sql('SELECT length(bytes) AS size FROM chunks WHERE image_id=? ORDER BY part', id)).map(row => row.size))
      .toEqual([1_000_000, 1_000_000, 300_000]);
    expectSameBytes(await bytesOf(await call(`/i/${id}.png`)), bytes);
  });
});

describe('image password header', () => {
  it.each([
    ['characters outside base64url', '!'.repeat(20)],
    ['fewer than 16 characters', encode('a'.repeat(11))],
    ['more than 172 characters', encode('a'.repeat(130))],
    ['an impossible base64 length', 'A'.repeat(17)],
    ['bytes that are not UTF-8', base64Url(new Uint8Array(12).fill(255))],
  ])('refuses %s with 400', async (_name, header) => {
    await expectRejected(await postImage(png(), { 'X-Image-Password': header }), 400, 'Invalid image password.');
  });

  it.each([
    ['fewer than 12 characters', '€€€€'],
    ['more than 128 characters', 'x'.repeat(129)],
    ['a newline', 'correct horse\nbattery'],
    ['a NUL', 'correct horse\0battery'],
    ['a DEL', 'correct horse\x7fbattery'],
  ])('refuses a password with %s', async (_name, password) => {
    await expectRejected(await postImage(png(), { 'X-Image-Password': encode(password) }), 400, PASSWORD_RULE);
  });

  it.each([['12 characters', 'x'.repeat(12)], ['128 characters', 'y'.repeat(128)]])('accepts %s', async (_name, password) => {
    const id = await upload(png(), { password });
    const [row] = await sql('SELECT password_salt, password_hash FROM images WHERE id=?', id);
    expect(row.password_salt).toMatch(/^[A-Za-z0-9_-]{22}$/);
    expect(row.password_hash).toMatch(/^[A-Za-z0-9_-]{43}$/);
  });

  it('is checked before the image is read', async () => {
    // Not an image: reading it would give "Invalid PNG image.".
    const response = await postImage(new Uint8Array(50).fill(9), { 'X-Image-Password': encode('short') });
    expect(response.status).toBe(400);
    expect((await response.json()).error).toBe('Invalid image password.');
  });
});

describe('an upload that races with the daily limit', () => {
  it('answers 429 and stores nothing when the day filled up while the image was still arriving', async () => {
    await putSettings({ daily_upload_limit: 1, limit_action: 'block_upload', warning_percent: 100 });
    let releaseBody, reportParked;
    const gate = new Promise(resolve => { releaseBody = resolve; });
    const parked = new Promise(resolve => { reportParked = resolve; });
    let sent = false;
    // The whole image arrives at once but the stream stays open: the daily limit has been
    // checked and the upload waits for the end of the body.
    const slow = new ReadableStream({
      async pull(controller) {
        if (!sent) { sent = true; controller.enqueue(png()); return; }
        reportParked();
        await gate;
        controller.close();
      },
    });
    const waiting = postImage(slow);
    await parked;
    const winner = await upload(png());
    releaseBody();
    const response = await waiting;
    expect(response.status).toBe(429);
    expect(response.headers.get('Content-Type')).toBe(JSON_TYPE);
    expect(await response.json()).toEqual({ error: LIMIT_MESSAGE });
    expect(Number(response.headers.get('Retry-After'))).toBeGreaterThan(0);
    expect((await sql('SELECT id FROM images')).map(row => row.id)).toEqual([winner]);
    expect(await sql('SELECT uploads FROM usage_daily')).toEqual([{ uploads: 1 }]);
  });
});

describe('storage faults', () => {
  const FULL = 'SQLITE_FULL: database or disk is full';
  const fillUp = table => sql(`CREATE TRIGGER disk_full BEFORE INSERT ON ${table} BEGIN SELECT RAISE(ABORT, '${FULL}'); END`);

  let errors;
  beforeEach(() => { errors = vi.spyOn(console, 'error').mockImplementation(() => {}); });
  afterEach(() => { vi.restoreAllMocks(); });

  it('answers an upload with 507 and rolls the whole upload back', async () => {
    await fillUp('chunks');
    await expectRejected(await postImage(png()), 507, 'Free storage is full. Delete images to make space.');
    expect(await sql('SELECT uploads FROM usage_daily')).toEqual([]);
    expect(errors).not.toHaveBeenCalled();
  });

  it('answers a viewer with a 507 page, not JSON', async () => {
    const id = await upload(png());
    await fillUp('view_marks');
    const response = await navigate(`/i/${id}`);
    expect(response.status).toBe(507);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(await response.text()).toContain('Temporarily unavailable');
  });

  it('answers any other storage failure with 503 and logs it once', async () => {
    await sql('DROP TABLE chunks');
    const response = await postImage(png());
    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ error: 'Service temporarily unavailable.' });
    expect(await sql('SELECT id FROM images')).toEqual([]);
    expect(errors).toHaveBeenCalledTimes(1);
  });
});

describe('trimming on upload', () => {
  it('keeps only the newest images within max_active and deletes the chunks of the others', async () => {
    await putSettings({ max_active: 2 });
    const first = await upload(png());
    const second = await upload(png());
    const third = await upload(png());
    expect((await sql('SELECT id FROM images ORDER BY rowid')).map(row => row.id)).toEqual([second, third]);
    const withChunks = (await sql('SELECT DISTINCT image_id FROM chunks')).map(row => row.image_id).sort();
    expect(withChunks).toEqual([second, third].sort());
    expect((await call(`/i/${first}`)).status).toBe(404);
  });
});
