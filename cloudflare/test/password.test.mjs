import { evictDurableObject, reset, runInDurableObject } from 'cloudflare:test';
import { beforeEach, describe, expect, it } from 'vitest';
import {
  PASSWORD, base64Url, bytesOf, call, cookieOf, navigate, png, sql, store, unlock, upload,
} from './helpers.mjs';

const ROBOTS = 'noindex, nofollow, noimageindex';
const WRONG = 'definitely not it';

beforeEach(async () => { await reset(); });

describe('password form', () => {
  it('is an English page that is never cached or indexed', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const response = await navigate(`/i/${id}`);
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(response.headers.get('Vary')).toBe('Cookie');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    expect(response.headers.get('X-Frame-Options')).toBe('DENY');
    expect(response.headers.get('Content-Security-Policy')).toBe(
      "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'");
    const html = await response.text();
    expect(html).toContain('<html lang="en">');
    expect(html).toContain('This screenshot is password protected');
    expect(html).toContain(`<form method="post" action="/i/${id}/unlock">`);
    expect(html).toContain('<label for="password">Password</label>');
    expect(html).toMatch(/<input id="password" name="password" type="password" required autofocus minlength="12" maxlength="128"/);
    expect(html).not.toContain('aria-invalid');
    expect(html).not.toContain('role="alert"');
  });

  it('is Turkish when the browser prefers Turkish', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const html = await (await navigate(`/i/${id}`, { 'Accept-Language': 'tr-TR,tr;q=0.9,en;q=0.8' })).text();
    expect(html).toContain('<html lang="tr">');
    expect(html).toContain('Bu ekran görüntüsü şifreli');
    expect(html).toContain('<label for="password">Şifre</label>');
  });

  it('is shown for the raw address too, because the cookie is scoped to /i/<id>', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const response = await call(`/i/${id}.png`);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(await response.text()).toContain(`action="/i/${id}/unlock"`);
  });
});

describe('wrong password', () => {
  it('re-renders the form with 403 and an announced inline error', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const response = await unlock(id, WRONG);
    expect(response.status).toBe(403);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(response.headers.get('Set-Cookie')).toBeNull();
    const html = await response.text();
    expect(html).toContain('<p class="error" id="error" role="alert">Wrong password. Check it and try again.</p>');
    expect(html).toContain('aria-invalid="true" aria-describedby="error"');
    expect(html).toContain(`action="/i/${id}/unlock"`);
  });

  it('is answered in Turkish for a Turkish browser', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const html = await (await unlock(id, WRONG, { 'Accept-Language': 'tr' })).text();
    expect(html).toContain('role="alert">Şifre yanlış. Kontrol edip yeniden deneyin.</p>');
  });

  it('treats a body that is not UTF-8 or a password over 128 characters as wrong', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const binary = await call(`/i/${id}/unlock`, { method: 'POST', body: Uint8Array.of(255, 254, 253) });
    expect(binary.status).toBe(403);
    const long = await unlock(id, 'x'.repeat(129));
    expect(long.status).toBe(403);
    expect(await long.text()).toContain('role="alert"');
  });

  it('rejects a body over 4 KiB with a 413 page', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const response = await unlock(id, 'x'.repeat(5000));
    expect(response.status).toBe(413);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(await response.text()).toContain('This request could not be processed.');
  });
});

describe('correct password', () => {
  it('redirects to the share with a signed, scoped, short-lived cookie', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const response = await unlock(id, PASSWORD);
    expect(response.status).toBe(303);
    expect(response.headers.get('Location')).toBe(`/i/${id}`);
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    expect(response.headers.get('Set-Cookie')).toMatch(
      new RegExp(`^u=\\d{10}\\.[A-Za-z0-9_-]{43}; Path=/i/${id}; Max-Age=3600; HttpOnly; Secure; SameSite=Lax$`));
    expect(await response.text()).toBe('');
    const expires = Number(cookieOf(response).slice(2).split('.')[0]);
    expect(expires - Date.now() / 1000).toBeGreaterThan(3590);
    expect(expires - Date.now() / 1000).toBeLessThanOrEqual(3600);
  });

  it('serves the image to a GET carrying the cookie, privately and without storing it', async () => {
    const bytes = png({ size: 300 });
    const id = await upload(bytes, { password: PASSWORD });
    const cookie = cookieOf(await unlock(id, PASSWORD));
    for (const path of [`/i/${id}`, `/i/${id}.png`]) {
      const response = await call(path, { headers: { Cookie: cookie, 'Sec-Fetch-Dest': 'document' } });
      expect(response.status).toBe(200);
      expect(response.headers.get('Content-Type')).toBe('image/png');
      expect(response.headers.get('Cache-Control')).toBe('private, no-store');
      expect(response.headers.get('Vary')).toBe('Cookie');
      expect(response.headers.get('Content-Length')).toBe('300');
      expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
      expect(response.headers.get('ETag')).toBeNull();
      expect(response.headers.get('Content-Disposition')).toMatch(/^inline; filename="isolmaSS-\d{4}-\d{2}-\d{2}_\d{2}-\d{2}\.png"$/);
      expect(await bytesOf(response)).toEqual(bytes);
    }
  });

  it('keeps working after the Durable Object restarts, because the signing key is stored', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const cookie = cookieOf(await unlock(id, PASSWORD));
    await evictDurableObject(store());
    const response = await call(`/i/${id}`, { headers: { Cookie: cookie } });
    expect(response.headers.get('Content-Type')).toBe('image/png');
  });

  it('is found among other cookies', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const cookie = cookieOf(await unlock(id, PASSWORD));
    const response = await call(`/i/${id}`, { headers: { Cookie: `theme=dark; ${cookie}; other=1` } });
    expect(response.headers.get('Content-Type')).toBe('image/png');
  });

  it('is refused when it is expired, altered, forged or issued for another share', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const other = await upload(png(), { password: PASSWORD });
    const [name, value] = cookieOf(await unlock(id, PASSWORD)).split('=');
    const [expires, signature] = value.split('.');
    const expired = await runInDurableObject(store(), async instance => {
      const past = Math.floor(Date.now() / 1000) - 10;
      return `u=${past}.${base64Url(await instance.unlockSignature(id, past))}`;
    });
    // The first character carries signature bits; the last one only has padding bits that decoding ignores.
    const wrongSignature = `${signature.startsWith('A') ? 'B' : 'A'}${signature.slice(1)}`;
    const attempts = [
      [id, expired],
      [id, `${name}=${Number(expires) + 1000}.${signature}`],
      [id, `${name}=${expires}.${wrongSignature}`],
      [id, `${name}=${expires}.short`],
      [other, `${name}=${value}`],
    ];
    for (const [target, cookie] of attempts) {
      const response = await call(`/i/${target}`, { headers: { Cookie: cookie } });
      expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
      expect(await response.text()).toContain(`action="/i/${target}/unlock"`);
    }
  });

  it('gives the attempt back, so a success never uses up the hour', async () => {
    const id = await upload(png(), { password: PASSWORD });
    for (let attempt = 0; attempt < 5; attempt++) await unlock(id, WRONG);
    expect((await unlock(id, PASSWORD)).status).toBe(303);
    const [row] = await sql('SELECT failures FROM password_attempts WHERE id=?', id);
    expect(row.failures).toBe(5);
  });

  it('redirects an unlock of an unprotected share to the share without a cookie', async () => {
    const id = await upload(png());
    const response = await unlock(id, PASSWORD);
    expect(response.status).toBe(303);
    expect(response.headers.get('Location')).toBe(`/i/${id}`);
    expect(response.headers.get('Set-Cookie')).toBeNull();
  });

  it('answers an unlock for an unknown share with the 404 page', async () => {
    const response = await unlock('A'.repeat(32), PASSWORD);
    expect(response.status).toBe(404);
    expect(await response.text()).toContain('This screenshot is no longer available');
    expect((await unlock('short', PASSWORD)).status).toBe(404);
  });
});

describe('attempt limit', () => {
  it('allows 20 wrong attempts per hour, then answers 429 with Retry-After, even for the right password', async () => {
    const id = await upload(png(), { password: PASSWORD });
    for (let attempt = 0; attempt < 20; attempt++) expect((await unlock(id, WRONG)).status).toBe(403);
    for (const password of [WRONG, PASSWORD]) {
      const response = await unlock(id, password);
      expect(response.status).toBe(429);
      expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
      expect(response.headers.get('Set-Cookie')).toBeNull();
      const wait = Number(response.headers.get('Retry-After'));
      expect(wait).toBeGreaterThan(0);
      expect(wait).toBeLessThanOrEqual(3600);
      const html = await response.text();
      expect(html).toContain('Too many attempts');
      expect(html).toContain('Try again in one hour.');
    }
  });

  it('is Turkish for a Turkish browser', async () => {
    const id = await upload(png(), { password: PASSWORD });
    for (let attempt = 0; attempt < 20; attempt++) await unlock(id, WRONG);
    const html = await (await unlock(id, WRONG, { 'Accept-Language': 'tr' })).text();
    expect(html).toContain('Bir saat sonra yeniden deneyin.');
  });

  it('holds when guesses arrive at the same time', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const responses = await Promise.all(Array.from({ length: 30 }, () => unlock(id, WRONG)));
    const statuses = responses.map(response => response.status);
    expect(statuses.filter(status => status === 403)).toHaveLength(20);
    expect(statuses.filter(status => status === 429)).toHaveLength(10);
  });

  it('counts each share separately', async () => {
    const first = await upload(png(), { password: PASSWORD });
    const second = await upload(png(), { password: PASSWORD });
    for (let attempt = 0; attempt < 21; attempt++) await unlock(first, WRONG);
    expect((await unlock(first, WRONG)).status).toBe(429);
    expect((await unlock(second, WRONG)).status).toBe(403);
  });
});
