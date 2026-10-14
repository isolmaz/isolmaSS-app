import { reset } from 'cloudflare:test';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PASSWORD, adminAuth, call, navigate, png, sql, untouchedStore, upload } from './helpers.mjs';

const ROBOTS = 'noindex, nofollow, noimageindex';
const HTML = 'text/html; charset=utf-8';

beforeEach(async () => { await reset(); });
afterEach(() => { vi.restoreAllMocks(); });

function countingLimiter(success = true) {
  const keys = [];
  return { keys, limit: async ({ key }) => { keys.push(key); return { success }; } };
}

describe('answers that never reach the Durable Object or the rate limiter', () => {
  const bindings = () => ({ STORE: untouchedStore, VIEW_LIMITER: countingLimiter() });

  it.each(['/favicon.ico', '/favicon.svg'])('serves the app icon at %s', async path => {
    const limiter = countingLimiter();
    const response = await call(path, {}, { STORE: untouchedStore, VIEW_LIMITER: limiter });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('image/svg+xml');
    expect(response.headers.get('Cache-Control')).toBe('public, max-age=604800');
    expect(response.headers.get('X-Content-Type-Options')).toBe('nosniff');
    const svg = await response.text();
    expect(svg).toContain('viewBox="0 0 256 256"');
    expect(svg).toContain('fill="#F59E0B"');
    expect(svg).toContain('stroke="#141413"');
    expect(limiter.keys).toEqual([]);
  });

  it('answers HEAD for the icon without a body', async () => {
    const response = await call('/favicon.ico', { method: 'HEAD' }, bindings());
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('image/svg+xml');
    expect(await response.text()).toBe('');
  });

  it('answers /robots.txt with a cheap 404 that does not hide /i/ from the noindex header', async () => {
    const response = await call('/robots.txt', {}, bindings());
    expect(response.status).toBe(404);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('public, max-age=86400');
    expect(await response.text()).not.toMatch(/disallow/i);
  });

  it('answers / with a noindex page in the visitor language', async () => {
    const response = await call('/', {}, bindings());
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe(HTML);
    expect(response.headers.get('Cache-Control')).toBe('public, max-age=3600');
    expect(response.headers.get('Vary')).toBe('Accept-Language');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    const html = await response.text();
    expect(html).toContain('Private isolmaSS share host');
    expect(html).toContain('<meta name="robots" content="noindex, nofollow, noimageindex">');
    const turkish = await (await call('/', { headers: { 'Accept-Language': 'tr' } }, bindings())).text();
    expect(turkish).toContain('Özel isolmaSS paylaşım sunucusu');
  });

  it('answers any other path with a 404 page', async () => {
    for (const method of ['GET', 'POST']) {
      const response = await call('/wp-login.php', { method }, bindings());
      expect(response.status).toBe(404);
      expect(response.headers.get('Content-Type')).toBe(HTML);
      expect(await response.text()).toContain('Page not found');
    }
  });

  it('names the allowed methods when the icon, robots.txt or / is posted to', async () => {
    for (const path of ['/favicon.ico', '/favicon.svg', '/robots.txt', '/']) {
      const response = await call(path, { method: 'POST' }, bindings());
      expect(response.status).toBe(405);
      expect(response.headers.get('Allow')).toBe('GET, HEAD');
      expect(response.headers.get('Content-Type')).toBe(HTML);
    }
  });
});

describe('rate limiting of viewers', () => {
  it('spends one token per share request, keyed by the visitor address', async () => {
    const id = await upload(png());
    const limiter = countingLimiter();
    const response = await call(`/i/${id}.png`, { headers: { 'CF-Connecting-IP': '198.51.100.4' } }, { VIEW_LIMITER: limiter });
    expect(response.status).toBe(200);
    expect(limiter.keys).toEqual(['198.51.100.4']);
    await call(`/i/${id}.png`, {}, { VIEW_LIMITER: limiter });
    expect(limiter.keys).toEqual(['198.51.100.4', 'unknown']);
  });

  it('answers an exhausted budget with an HTML page and Retry-After, before the Durable Object', async () => {
    const response = await call('/i/' + 'C'.repeat(32), {}, { STORE: untouchedStore, VIEW_LIMITER: countingLimiter(false) });
    expect(response.status).toBe(429);
    expect(response.headers.get('Content-Type')).toBe(HTML);
    expect(response.headers.get('Retry-After')).toBe('60');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    const html = await response.text();
    expect(html).toContain('Too many requests');
    expect(html).toContain('Try again in a minute.');
    expect(html).not.toContain('"error"');
    const turkish = await call('/i/' + 'C'.repeat(32), { headers: { 'Accept-Language': 'tr' } },
      { STORE: untouchedStore, VIEW_LIMITER: countingLimiter(false) });
    expect(await turkish.text()).toContain('Bir dakika sonra yeniden deneyin.');
  });

  it('limits unlocking as well', async () => {
    const response = await call('/i/' + 'C'.repeat(32) + '/unlock', { method: 'POST' },
      { STORE: untouchedStore, VIEW_LIMITER: countingLimiter(false) });
    expect(response.status).toBe(429);
    expect(response.headers.get('Content-Type')).toBe(HTML);
  });

  it('does not limit the API', async () => {
    const limiter = countingLimiter(false);
    const response = await call('/api/stats', { headers: adminAuth() }, { VIEW_LIMITER: limiter });
    expect(response.status).toBe(200);
    expect(limiter.keys).toEqual([]);
  });
});

describe('expired, deleted and unknown shares', () => {
  it('shows the "no longer available" page in English and Turkish', async () => {
    const response = await call(`/i/${'D'.repeat(32)}`);
    expect(response.status).toBe(404);
    expect(response.headers.get('Content-Type')).toBe(HTML);
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    const html = await response.text();
    expect(html).toContain('<html lang="en">');
    expect(html).toContain('This screenshot is no longer available');
    expect(html).toContain('It expired or its owner deleted it.');
    expect(html).not.toContain('"error"');
    const turkish = await (await call(`/i/${'D'.repeat(32)}`, { headers: { 'Accept-Language': 'tr-TR,tr;q=0.9' } })).text();
    expect(turkish).toContain('<html lang="tr">');
    expect(turkish).toContain('Bu ekran görüntüsü artık yok');
    expect(turkish).toContain('Süresi doldu ya da paylaşan kişi sildi.');
  });

  it('shows it for an expired share of either kind', async () => {
    const open = await upload(png());
    const protectedId = await upload(png(), { password: PASSWORD });
    await sql('UPDATE images SET expires_at=?', Math.floor(Date.now() / 1000) - 5);
    for (const id of [open, protectedId]) {
      const response = await call(`/i/${id}`);
      expect(response.status).toBe(404);
      expect(await response.text()).toContain('no longer available');
    }
  });

  it('shows it for malformed share addresses', async () => {
    for (const path of ['/i/', '/i/short', `/i/${'E'.repeat(33)}`, '/i/a/b/c', `/i/${'E'.repeat(31)}!`]) {
      const response = await call(path);
      expect(response.status).toBe(404);
      expect(response.headers.get('Content-Type')).toBe(HTML);
    }
  });
});

describe('failures behind the edge', () => {
  it('shows a viewer an HTML page and the app JSON when the Durable Object is unreachable', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const page = await call(`/i/${'F'.repeat(32)}`, {}, { STORE: untouchedStore });
    expect(page.status).toBe(503);
    expect(page.headers.get('Content-Type')).toBe(HTML);
    expect(await page.text()).toContain('Temporarily unavailable');
    const api = await call('/api/stats', { headers: adminAuth() }, { STORE: untouchedStore });
    expect(api.status).toBe(503);
    expect(api.headers.get('Content-Type')).toBe('application/json; charset=utf-8');
    expect(await api.json()).toEqual({ error: 'Service temporarily unavailable.' });
  });

  it('shows a viewer an HTML page and the app JSON for a hostname the Worker does not serve', async () => {
    const page = await call('http://share.test/i/' + 'G'.repeat(32));
    expect(page.status).toBe(421);
    expect(page.headers.get('Content-Type')).toBe(HTML);
    const api = await call('http://share.test/api/stats', { headers: adminAuth() });
    expect(api.status).toBe(421);
    expect(await api.json()).toEqual({ error: 'Unknown or insecure hostname.' });
  });

  it('reports an unfinished image as an unavailable page', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const id = await upload(png());
    await sql('DELETE FROM chunks WHERE image_id=?', id);
    const response = await call(`/i/${id}.png`);
    expect(response.status).toBe(503);
    expect(response.headers.get('Content-Type')).toBe(HTML);
  });
});

describe('every page', () => {
  it('has the Windows 11 look, a favicon, light and dark themes and no script', async () => {
    const id = await upload(png());
    const protectedId = await upload(png(), { password: PASSWORD });
    const pages = [
      await call('/'),
      await call(`/i/${'H'.repeat(32)}`),
      await call(`/i/${protectedId}`),
      await navigate(`/i/${id}`),
    ];
    for (const page of pages) {
      const html = await page.text();
      expect(page.headers.get('X-Robots-Tag')).toBe(ROBOTS);
      expect(html).toContain('<meta name="color-scheme" content="light dark">');
      expect(html).toContain('<meta name="robots" content="noindex, nofollow, noimageindex">');
      expect(html).toContain('<meta name="viewport" content="width=device-width,initial-scale=1">');
      expect(html).toMatch(/<link rel="icon" type="image\/svg\+xml" href="data:image\/svg\+xml,%3Csvg/);
      expect(html).toContain('font:16px/1.5 "Segoe UI Variable Text","Segoe UI",system-ui,sans-serif');
      expect(html).toContain('--bg:#F3F3F3;--card:#FFFFFF;--fg:#1A1A1A;--muted:#5D5D5D;--line:#E5E5E5');
      expect(html).toContain('@media (prefers-color-scheme:dark){:root{--bg:#202020;--card:#2B2B2B;--fg:#FFFFFF;--muted:#C5C5C5;--line:#3A3A3A');
      expect(html).toContain('--accent:#F59E0B;--accent-hover:#F7AD33;--accent-pressed:#D98A06;--ink:#141413');
      expect(html).toContain(':focus-visible{outline:2px solid var(--fg);outline-offset:2px}');
      expect(html).toContain('border-radius:8px');
      expect(html).toContain('border-radius:4px');
      expect(html).toContain('<footer>Shared with isolmaSS</footer>');
      expect(html).not.toContain('<script');
      expect(page.headers.get('Content-Security-Policy')).toMatch(/^default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'; form-action '(self|none)'$/);
    }
  });

  it('has a Turkish footer for a Turkish browser', async () => {
    const html = await (await call('/', { headers: { 'Accept-Language': 'tr' } })).text();
    expect(html).toContain('<footer>isolmaSS ile paylaşıldı</footer>');
  });
});
