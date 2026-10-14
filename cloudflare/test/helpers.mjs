import { env } from 'cloudflare:workers';
import { runInDurableObject } from 'cloudflare:test';
import { expect } from 'vitest';
import worker from '../worker.mjs';

export const ORIGIN = 'https://share.test';
export const PASSWORD = 'correct horse battery';

export function base64Url(bytes) {
  return btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
}

export const store = () => env.STORE.getByName('installation');
export const uploadAuth = () => ({ Authorization: `Bearer ${env.UPLOAD_TOKEN}` });
export const adminAuth = () => ({ Authorization: `Bearer ${env.ADMIN_TOKEN}` });

/**
 * Calls the Worker's exported handler with the real Durable Object. The
 * rate-limiter binding is left out unless a test supplies one, so tests never
 * share a per-IP budget.
 */
export function call(path, init = {}, bindings = {}) {
  const url = path.startsWith('http') ? path : `${ORIGIN}${path}`;
  const request = new Request(url, { redirect: 'manual', ...init });
  return worker.fetch(request, {
    STORE: env.STORE, UPLOAD_TOKEN: env.UPLOAD_TOKEN, ADMIN_TOKEN: env.ADMIN_TOKEN, ...bindings,
  });
}

/** A request from a browser navigating to the link. */
export const navigate = (path, headers = {}) => call(path, { headers: { 'Sec-Fetch-Dest': 'document', ...headers } });

/** The smallest bytes the Worker accepts as a PNG: signature, IHDR with the given size, filler. */
export function png({ width = 640, height = 480, size = 64 } = {}) {
  const bytes = new Uint8Array(size).fill(7);
  bytes.set([137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82]);
  const view = new DataView(bytes.buffer);
  view.setUint32(16, width);
  view.setUint32(20, height);
  return bytes;
}

export function jpeg(size = 64) {
  const bytes = new Uint8Array(size).fill(1);
  bytes.set([255, 216, 255]);
  bytes.set([255, 217], size - 2);
  return bytes;
}

export async function upload(bytes, { type = 'image/png', password } = {}) {
  const headers = { ...uploadAuth(), 'Content-Type': type };
  if (password) headers['X-Image-Password'] = base64Url(new TextEncoder().encode(password));
  const response = await call('/api/upload', { method: 'POST', headers, body: bytes });
  expect(response.status).toBe(201);
  return (await response.json()).id;
}

export function unlock(id, password, headers = {}) {
  return call(`/i/${id}/unlock`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded', ...headers },
    body: new URLSearchParams({ password }).toString(),
  });
}

export async function admin(path, init = {}) {
  return call(path, { ...init, headers: { ...adminAuth(), ...init.headers } });
}

export async function putSettings(settings) {
  const response = await admin('/api/settings', { method: 'PUT', body: JSON.stringify(settings) });
  expect(response.status).toBe(200);
}

/** Runs SQL inside the Durable Object, for state the API does not expose. */
export function sql(query, ...bindings) {
  return runInDurableObject(store(), instance => instance.sql.exec(query, ...bindings).toArray());
}

/** The "name=value" part of a Set-Cookie header, as a browser would send it back. */
export function cookieOf(response) {
  return response.headers.get('Set-Cookie').split(';')[0];
}

export async function bytesOf(response) {
  return new Uint8Array(await response.arrayBuffer());
}

/** Bindings that fail the test when the Durable Object is touched. */
export const untouchedStore = { getByName() { throw new Error('The Durable Object must not be used.'); } };
