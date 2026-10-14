import { reset, runInDurableObject } from 'cloudflare:test';
import { beforeEach, describe, expect, it } from 'vitest';
import { PASSWORD, admin, bytesOf, call, expectSameBytes, jpeg, navigate, png, sql, store, upload } from './helpers.mjs';

const ROBOTS = 'noindex, nofollow, noimageindex';
const THIRTY_DAYS = 30 * 86400;

beforeEach(async () => { await reset(); });

async function views() {
  return (await (await admin('/api/stats')).json()).daily.views;
}

/** isolmaSS-YYYY-MM-DD_HH-MM in UTC, built from the stored creation time. */
async function expectedStem(id) {
  const [{ created_at }] = await sql('SELECT created_at FROM images WHERE id=?', id);
  const iso = new Date(created_at * 1000).toISOString();
  return `isolmaSS-${iso.slice(0, 10)}_${iso.slice(11, 13)}-${iso.slice(14, 16)}`;
}

describe('viewer page', () => {
  it('describes the share for browsers and unfurlers', async () => {
    const id = await upload(png({ width: 640, height: 480 }));
    const response = await navigate(`/i/${id}`);
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    const policy = response.headers.get('Content-Security-Policy');
    const [, nonce] = /script-src 'nonce-([A-Za-z0-9_-]{32})'/.exec(policy);
    expect(policy).toBe(
      `default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'; script-src 'nonce-${nonce}'; form-action 'none'`);
    const html = await response.text();
    const raw = `/i/${id}.png`;
    expect(html).toContain('<html lang="en">');
    expect(html).toContain('<title>Screenshot · isolmaSS</title>');
    expect(html).toContain('<meta name="robots" content="noindex, nofollow, noimageindex">');
    expect(html).toContain('<meta property="og:type" content="website">');
    expect(html).toContain(`<meta property="og:image" content="https://share.test${raw}">`);
    expect(html).toContain('<meta property="og:image:type" content="image/png">');
    expect(html).toContain('<meta property="og:image:width" content="640">');
    expect(html).toContain('<meta property="og:image:height" content="480">');
    expect(html).toContain('<meta name="twitter:card" content="summary_large_image">');
    expect(html).toContain(`<meta name="twitter:image" content="https://share.test${raw}">`);
    expect(html).toContain(`<a class="shot" href="${raw}"><img src="${raw}" alt="Shared screenshot" width="640" height="480"></a>`);
    expect(html).toContain(`<a class="btn" href="${raw}" target="_blank" rel="noopener">Open original</a>`);
    expect(html).toContain(`<a class="btn" href="/i/${id}?download=1">Download</a>`);
    expect(html.match(/<script/g)).toHaveLength(1);
    expect(html).toContain(`<script nonce="${nonce}">`);
  });

  it('is Turkish for a Turkish browser', async () => {
    const id = await upload(png());
    const html = await (await navigate(`/i/${id}`, { 'Accept-Language': 'tr-TR' })).text();
    expect(html).toContain('<html lang="tr">');
    expect(html).toContain('>İndir</a>');
    expect(html).toContain('>Orijinali aç</a>');
    expect(html).toContain('alt="Paylaşılan ekran görüntüsü"');
    expect(html).toContain('data-done="Kopyalandı" data-failed="Kopyalanamadı">Bağlantıyı kopyala</button>');
    expect(html).toContain(`title="Kötüye kullanımı Cloudflare'e bildir" aria-label="Kötüye kullanımı Cloudflare'e bildir">Bildir</a>`);
    expect(html).not.toContain('Download');
    expect(html).not.toContain('Report');
  });

  it('leaves the size out for JPEGs, whose dimensions the Worker does not read', async () => {
    const id = await upload(jpeg(), { type: 'image/jpeg' });
    const html = await (await navigate(`/i/${id}`)).text();
    expect(html).toContain(`<meta property="og:image" content="https://share.test/i/${id}.jpg">`);
    expect(html).toContain('<meta property="og:image:type" content="image/jpeg">');
    expect(html).not.toContain('og:image:width');
    expect(html).not.toMatch(/<img [^>]*width=/);
    expect(html).toContain(`<a class="btn" href="/i/${id}.jpg" target="_blank" rel="noopener">Open original</a>`);
  });

  it('escapes the address it embeds in the og: tags, whatever host the Durable Object is given', async () => {
    // The runtime only forwards hostnames made of letters, digits, hyphens and underscores to the
    // Durable Object, so this calls its fetch directly. Quotes, ampersands and apostrophes are valid
    // in a hostname, and none of them may break out of an attribute.
    const host = `a&b'c"d.test`;
    const id = await upload(png());
    const html = await runInDurableObject(store(), async instance => {
      const request = new Request(`https://${host}/i/${id}`, { headers: { 'Sec-Fetch-Dest': 'document' } });
      const response = await instance.fetch(request);
      expect(response.status).toBe(200);
      return await response.text();
    });
    const escaped = 'a&amp;b&#39;c&quot;d.test';
    expect(html).toContain(`<meta property="og:image" content="https://${escaped}/i/${id}.png">`);
    expect(html).toContain(`<meta name="twitter:image" content="https://${escaped}/i/${id}.png">`);
    expect(html).not.toContain(host);
  });

  it('is built from the stored size, which an older Worker never recorded', async () => {
    const id = await upload(png({ width: 800, height: 600 }));
    await sql('UPDATE images SET width=NULL, height=NULL WHERE id=?', id);
    const html = await (await navigate(`/i/${id}`)).text();
    expect(html).not.toContain('og:image:width');
    await sql('UPDATE images SET width=?, height=? WHERE id=?', 1920, 1080, id);
    expect(await (await navigate(`/i/${id}`)).text()).toContain('<meta property="og:image:width" content="1920">');
  });
});

describe('viewer branding and actions', () => {
  it('links the product, its download page, the abuse form and a footer to the site', async () => {
    const id = await upload(png());
    const html = await (await navigate(`/i/${id}`)).text();
    expect(html).toMatch(/<header class="top"><a class="brand" href="https:\/\/ss\.isolmaz\.com"><svg [^>]*aria-hidden="true"[^>]*>.*?<\/svg>isolmaSS<\/a>/);
    expect(html).toContain('<a class="btn primary" href="https://ss.isolmaz.com/download">Get isolmaSS — free</a>');
    expect(html).toContain('<a class="btn quiet end" href="https://abuse.cloudflare.com/" target="_blank" rel="noopener noreferrer" title="Report abuse to Cloudflare" aria-label="Report abuse to Cloudflare">Report</a>');
    expect(html).toContain('<footer>Shared with <a href="https://ss.isolmaz.com">isolmaSS</a> · Free screenshot tool for Windows</footer>');
  });

  it('is Turkish in the bar and the footer for a Turkish browser, never mixing languages', async () => {
    const id = await upload(png());
    const html = await (await navigate(`/i/${id}`, { 'Accept-Language': 'tr-TR,tr;q=0.9,en;q=0.8' })).text();
    expect(html).toContain(`<a class="btn primary" href="https://ss.isolmaz.com/download">isolmaSS'i ücretsiz edinin</a>`);
    expect(html).toContain('<footer><a href="https://ss.isolmaz.com">isolmaSS</a> ile paylaşıldı · Windows için ücretsiz ekran görüntüsü aracı</footer>');
    expect(html).not.toContain('Get isolmaSS');
    expect(html).not.toContain('Free screenshot tool');
    expect(html).not.toContain('Copy link');
    expect(html).not.toContain('Copy failed');
  });

  it('follows the first language of Accept-Language only', async () => {
    const id = await upload(png());
    const html = await (await navigate(`/i/${id}`, { 'Accept-Language': 'de-DE,tr;q=0.8' })).text();
    expect(html).toContain('<html lang="en">');
    expect(html).toContain('>Copy link</button>');
  });

  it('keeps Copy link hidden until the script finds the Clipboard API, and copies the page address', async () => {
    const id = await upload(png());
    const html = await (await navigate(`/i/${id}`)).text();
    expect(html).toContain(`<button class="btn" id="copy" type="button" hidden aria-live="polite" data-url="https://share.test/i/${id}" data-done="Copied" data-failed="Copy failed">Copy link</button>`);
    const script = /<script nonce="[^"]+">(.*?)<\/script>/.exec(html)[1];
    expect(script).toContain("document.getElementById('copy')");
    expect(script).toContain('if(!navigator.clipboard)return;b.hidden=false');
    expect(script).toContain('navigator.clipboard.writeText(b.dataset.url)');
    // A refused write must end in the "Copy failed" label, not in an unhandled rejection.
    expect(script).toContain('.then(()=>show(b.dataset.done)).catch(()=>show(b.dataset.failed))');
    expect(script).toContain('b.textContent=text;clearTimeout(timer);timer=setTimeout(()=>{b.textContent=label},2000)');
    expect(html).toContain('.btn[hidden]{display:none}');
  });

  it('uses a fresh nonce for every page, the same one in the header and the script tag', async () => {
    const id = await upload(png());
    const nonces = [];
    for (let visit = 0; visit < 2; visit++) {
      const response = await navigate(`/i/${id}`);
      const [, inPolicy] = /script-src 'nonce-([^']+)'/.exec(response.headers.get('Content-Security-Policy'));
      const [, inPage] = /<script nonce="([^"]+)">/.exec(await response.text());
      expect(inPage).toBe(inPolicy);
      nonces.push(inPage);
    }
    expect(nonces[0]).not.toBe(nonces[1]);
  });

  it('loads nothing from another site and sets no cookie', async () => {
    const id = await upload(png());
    const response = await navigate(`/i/${id}`);
    const html = await response.text();
    expect(response.headers.get('Set-Cookie')).toBeNull();
    expect(html).not.toMatch(/<script[^>]*\ssrc=/);
    expect(html).not.toMatch(/<(?:iframe|object|embed|form)\b/);
    expect(html).not.toMatch(/@import|url\(/);
    const loaded = [...html.matchAll(/<(?:img|link)\b[^>]*\s(?:src|href)="([^"]*)"/g)].map(match => match[1]);
    expect(loaded).toHaveLength(2);
    expect(loaded.every(url => url.startsWith('/') || url.startsWith('data:'))).toBe(true);
  });
});

describe('download variant', () => {
  const attachment = async id => `attachment; filename="${await expectedStem(id)}.png"`;

  it.each([
    ['the page address', id => `/i/${id}?download=1`],
    ['the raw address', id => `/i/${id}.png?download=1`],
  ])('serves the bytes as an attachment at %s, even to a navigation', async (_name, path) => {
    const bytes = png({ size: 3000 });
    const id = await upload(bytes);
    const response = await navigate(path(id));
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Disposition')).toBe(await attachment(id));
    expect(response.headers.get('Content-Type')).toBe('image/png');
    expect(response.headers.get('Content-Length')).toBe('3000');
    expect(response.headers.get('Content-Security-Policy')).toBe("default-src 'none'; sandbox");
    expect(response.headers.get('X-Content-Type-Options')).toBe('nosniff');
    expect(response.headers.get('Referrer-Policy')).toBe('no-referrer');
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    expect(response.headers.get('ETag')).toBe(`"${id}"`);
    expect(response.headers.get('Cache-Control')).toMatch(/^private, max-age=\d+, immutable$/);
    expect(await bytesOf(response)).toEqual(bytes);
  });

  it('names a JPEG after its format', async () => {
    const bytes = jpeg(200);
    const id = await upload(bytes, { type: 'image/jpeg' });
    const response = await navigate(`/i/${id}?download=1`);
    expect(response.headers.get('Content-Type')).toBe('image/jpeg');
    expect(response.headers.get('Content-Disposition')).toBe(`attachment; filename="${await expectedStem(id)}.jpg"`);
    expect(await bytesOf(response)).toEqual(bytes);
  });

  it('is the only way to get an attachment: the plain address and other values stay inline', async () => {
    const id = await upload(png());
    expect((await call(`/i/${id}.png`)).headers.get('Content-Disposition')).toBe(`inline; filename="${await expectedStem(id)}.png"`);
    for (const query of ['download=0', 'download=true', 'download=', 'download=11', 'other=1']) {
      const page = await navigate(`/i/${id}?${query}`);
      expect(page.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
      expect(page.headers.get('Content-Disposition')).toBeNull();
      const raw = await call(`/i/${id}.png?${query}`);
      expect(raw.headers.get('Content-Disposition')).toMatch(/^inline;/);
    }
  });

  it('answers HEAD with the attachment headers, no body and no view', async () => {
    const id = await upload(png({ size: 400 }));
    const response = await call(`/i/${id}?download=1`, { method: 'HEAD', headers: { 'Sec-Fetch-Dest': 'document' } });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('image/png');
    expect(response.headers.get('Content-Length')).toBe('400');
    expect(response.headers.get('Content-Disposition')).toBe(await attachment(id));
    expect(await response.text()).toBe('');
    expect(await views()).toBe(0);
  });

  it('counts a visitor once per image and day, like the image itself', async () => {
    const id = await upload(png());
    const visitor = { 'CF-Connecting-IP': '203.0.113.9' };
    await call(`/i/${id}?download=1`, { headers: visitor });
    await call(`/i/${id}.png`, { headers: visitor });
    expect(await views()).toBe(1);
    await call(`/i/${id}?download=1`, { headers: { 'CF-Connecting-IP': '203.0.113.10' } });
    expect(await views()).toBe(2);
  });

  it('is the 404 page for an unknown, deleted or expired share and for the wrong extension', async () => {
    const deleted = await upload(png());
    const expired = await upload(png());
    const live = await upload(png());
    expect((await admin(`/api/images/${deleted}`, { method: 'DELETE' })).status).toBe(200);
    await sql('UPDATE images SET expires_at=? WHERE id=?', Math.floor(Date.now() / 1000) - 1, expired);
    for (const path of [`/i/${'B'.repeat(32)}?download=1`, `/i/${deleted}?download=1`, `/i/${expired}?download=1`,
      `/i/${expired}.png?download=1`, `/i/${live}.jpg?download=1`, `/i/short?download=1`]) {
      const response = await navigate(path);
      expect(response.status).toBe(404);
      expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
      expect(response.headers.get('Content-Disposition')).toBeNull();
      expect(await response.text()).toContain('This screenshot is no longer available');
    }
  });

  it('is only for GET and HEAD', async () => {
    const id = await upload(png());
    const response = await call(`/i/${id}?download=1`, { method: 'POST' });
    expect(response.status).toBe(405);
    expect(response.headers.get('Allow')).toBe('GET, HEAD');
    expect(response.headers.get('Content-Disposition')).toBeNull();
  });
});

describe('raw bytes versus page', () => {
  it('serves the bytes inline for ?raw=1 at the page address, even to a navigation', async () => {
    const bytes = png({ size: 128 });
    const id = await upload(bytes);
    const response = await navigate(`/i/${id}?raw=1`);
    expect(response.headers.get('Content-Type')).toBe('image/png');
    expect(response.headers.get('Content-Disposition')).toBe(`inline; filename="${await expectedStem(id)}.png"`);
    expect(await bytesOf(response)).toEqual(bytes);
    expect((await navigate(`/i/${id}?raw=0`)).headers.get('Content-Type')).toBe('text/html; charset=utf-8');
  });

  it.each([
    { name: 'no hints', headers: {}, expected: 'image' },
    { name: 'an <img> embed', headers: { 'Sec-Fetch-Dest': 'image' }, expected: 'image' },
    { name: 'an <img> embed whose Accept mentions HTML', headers: { 'Sec-Fetch-Dest': 'image', Accept: 'text/html' }, expected: 'image' },
    { name: 'an iframe', headers: { 'Sec-Fetch-Dest': 'iframe', Accept: 'text/html' }, expected: 'image' },
    { name: 'a navigation', headers: { 'Sec-Fetch-Dest': 'document' }, expected: 'page' },
    { name: 'an old browser asking for HTML', headers: { Accept: 'text/html,application/xhtml+xml;q=0.9,*/*;q=0.8' }, expected: 'page' },
    { name: 'an image request without Sec-Fetch-Dest', headers: { Accept: 'image/avif,image/webp,*/*' }, expected: 'image' },
  ])('answers $name with the $expected', async ({ headers, expected }) => {
    const bytes = png({ size: 128 });
    const id = await upload(bytes);
    const response = await call(`/i/${id}`, { headers });
    if (expected === 'image') {
      expect(response.headers.get('Content-Type')).toBe('image/png');
      expect(await bytesOf(response)).toEqual(bytes);
    } else {
      expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    }
  });

  it('always serves the bytes at the .png address, even to a navigation', async () => {
    const bytes = png({ size: 128 });
    const id = await upload(bytes);
    const response = await navigate(`/i/${id}.png`);
    expect(response.headers.get('Content-Type')).toBe('image/png');
    expect(await bytesOf(response)).toEqual(bytes);
  });

  it('has no raw address for another format or a missing extension match', async () => {
    const pngId = await upload(png());
    const jpegId = await upload(jpeg(), { type: 'image/jpeg' });
    for (const path of [`/i/${pngId}.jpg`, `/i/${jpegId}.png`, `/i/${pngId}.gif`, `/i/${pngId}.png.png`]) {
      const response = await call(path);
      expect(response.status).toBe(404);
      expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    }
    expect((await call(`/i/${jpegId}.jpg`)).headers.get('Content-Type')).toBe('image/jpeg');
  });
});

describe('raw image response', () => {
  it('has a known length, a dated filename and private caching bounded by the expiry', async () => {
    const bytes = png({ size: 3000 });
    const id = await upload(bytes);
    const response = await call(`/i/${id}.png`);
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Length')).toBe('3000');
    expect(response.headers.get('Content-Type')).toBe('image/png');
    expect(response.headers.get('Content-Disposition')).toBe(`inline; filename="${await expectedStem(id)}.png"`);
    expect(response.headers.get('Content-Security-Policy')).toBe("default-src 'none'; sandbox");
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    expect(response.headers.get('X-Content-Type-Options')).toBe('nosniff');
    expect(response.headers.get('Referrer-Policy')).toBe('no-referrer');
    expect(response.headers.get('ETag')).toBe(`"${id}"`);
    expect(response.headers.get('Vary')).toBe('Accept, Sec-Fetch-Dest');
    const cache = response.headers.get('Cache-Control');
    expect(cache).not.toContain('public');
    const [, seconds] = /^private, max-age=(\d+), immutable$/.exec(cache);
    expect(Number(seconds)).toBeLessThanOrEqual(THIRTY_DAYS);
    expect(Number(seconds)).toBeGreaterThan(THIRTY_DAYS - 60);
    expect(await bytesOf(response)).toEqual(bytes);
  });

  it('sends a multi-chunk image intact', async () => {
    const bytes = png({ size: 2_500_000 });
    for (let index = 24; index < bytes.length; index++) bytes[index] = index % 251;
    const id = await upload(bytes);
    const response = await call(`/i/${id}.png`);
    expect(response.headers.get('Content-Length')).toBe('2500000');
    expectSameBytes(await bytesOf(response), bytes);
  });

  it('stops being cached when the image expires sooner than the default', async () => {
    const id = await upload(png());
    await sql('UPDATE images SET expires_at=? WHERE id=?', Math.floor(Date.now() / 1000) + 120, id);
    const cache = (await call(`/i/${id}.png`)).headers.get('Cache-Control');
    const [, seconds] = /^private, max-age=(\d+), immutable$/.exec(cache);
    expect(Number(seconds)).toBeGreaterThan(100);
    expect(Number(seconds)).toBeLessThanOrEqual(120);
  });

  it('answers a matching If-None-Match with 304 and the same bounded caching', async () => {
    const id = await upload(png());
    const fresh = await call(`/i/${id}.png`);
    const response = await call(`/i/${id}.png`, { headers: { 'If-None-Match': `"${id}"` } });
    expect(response.status).toBe(304);
    expect(await response.text()).toBe('');
    const [, revalidated] = /^private, max-age=(\d+), immutable$/.exec(response.headers.get('Cache-Control'));
    const [, original] = /^private, max-age=(\d+), immutable$/.exec(fresh.headers.get('Cache-Control'));
    expect(Math.abs(Number(revalidated) - Number(original))).toBeLessThanOrEqual(2);
    expect(response.headers.get('ETag')).toBe(`"${id}"`);
    expect(response.headers.get('X-Robots-Tag')).toBe(ROBOTS);
    expect((await call(`/i/${id}.png`, { headers: { 'If-None-Match': '"other"' } })).status).toBe(200);
  });

  it('never revalidates a deleted or expired image', async () => {
    const deleted = await upload(png());
    const expired = await upload(png());
    expect((await admin(`/api/images/${deleted}`, { method: 'DELETE' })).status).toBe(200);
    await sql('UPDATE images SET expires_at=? WHERE id=?', Math.floor(Date.now() / 1000) - 1, expired);
    for (const id of [deleted, expired]) {
      const response = await call(`/i/${id}.png`, { headers: { 'If-None-Match': `"${id}"` } });
      expect(response.status).toBe(404);
      expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    }
  });

  it('answers a revalidating navigation with the page, not a 304', async () => {
    const id = await upload(png());
    const response = await navigate(`/i/${id}`, { 'If-None-Match': `"${id}"` });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
  });
});

describe('HEAD', () => {
  it('describes the image without a body or a view', async () => {
    const id = await upload(png({ size: 400 }));
    const response = await call(`/i/${id}.png`, { method: 'HEAD' });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('image/png');
    expect(response.headers.get('Content-Length')).toBe('400');
    expect(response.headers.get('Cache-Control')).toMatch(/^private, max-age=\d+, immutable$/);
    expect(response.headers.get('ETag')).toBe(`"${id}"`);
    expect(await response.text()).toBe('');
    expect(await views()).toBe(0);
  });

  it('describes the page of a navigation without a view', async () => {
    const id = await upload(png());
    const response = await call(`/i/${id}`, { method: 'HEAD', headers: { 'Sec-Fetch-Dest': 'document' } });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(await response.text()).toBe('');
    expect(await views()).toBe(0);
  });

  it('describes the password form of a protected share', async () => {
    const id = await upload(png(), { password: PASSWORD });
    const response = await call(`/i/${id}`, { method: 'HEAD' });
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('no-store, max-age=0');
    expect(await response.text()).toBe('');
  });

  it('honors If-None-Match', async () => {
    const id = await upload(png());
    const response = await call(`/i/${id}`, { method: 'HEAD', headers: { 'If-None-Match': `"${id}"` } });
    expect(response.status).toBe(304);
  });

  it('reports a missing share as 404 without a body', async () => {
    const response = await call(`/i/${'B'.repeat(32)}`, { method: 'HEAD' });
    expect(response.status).toBe(404);
    expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    expect(await response.text()).toBe('');
  });

  it('names the allowed methods when another one is used', async () => {
    const id = await upload(png());
    const post = await call(`/i/${id}`, { method: 'POST' });
    expect(post.status).toBe(405);
    expect(post.headers.get('Allow')).toBe('GET, HEAD');
    expect(post.headers.get('Content-Type')).toBe('text/html; charset=utf-8');
    const get = await call(`/i/${id}/unlock`);
    expect(get.status).toBe(405);
    expect(get.headers.get('Allow')).toBe('POST');
    expect(await get.text()).toContain('This request could not be processed.');
  });
});

describe('view counting', () => {
  it('counts a visitor once per image and day, whether they open the page or the bytes', async () => {
    const id = await upload(png());
    const visitor = { 'CF-Connecting-IP': '203.0.113.7' };
    await call(`/i/${id}`, { headers: { ...visitor, 'Sec-Fetch-Dest': 'document' } });
    expect(await views()).toBe(1);
    await call(`/i/${id}.png`, { headers: visitor });
    await call(`/i/${id}`, { headers: { ...visitor, 'Sec-Fetch-Dest': 'image' } });
    expect(await views()).toBe(1);
    await call(`/i/${id}.png`, { headers: { 'CF-Connecting-IP': '203.0.113.8' } });
    expect(await views()).toBe(2);
    const [{ views: perImage }] = await sql('SELECT views FROM images WHERE id=?', id);
    expect(perImage).toBe(2);
  });

  it('does not count a 304', async () => {
    const id = await upload(png());
    await call(`/i/${id}.png`, { headers: { 'If-None-Match': `"${id}"` } });
    expect(await views()).toBe(0);
  });
});
