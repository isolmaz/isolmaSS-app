import { cloudflareTest } from '@cloudflare/vitest-plugin';
import { defineConfig } from 'vitest/config';

// The tests run in workerd with the real ShareStore Durable Object (SQLite),
// configured by wrangler.jsonc. The secrets are 43-character base64url
// values, the only shape the Worker accepts.
export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: './wrangler.jsonc' },
      miniflare: {
        bindings: {
          UPLOAD_TOKEN: 'u'.repeat(43),
          ADMIN_TOKEN: 'a'.repeat(43),
        },
      },
    }),
  ],
  test: {
    include: ['test/**/*.test.mjs'],
    coverage: {
      provider: 'istanbul',
      include: ['worker.mjs'],
      reporter: ['text', 'lcov'],
      reportsDirectory: './coverage',
      thresholds: { lines: 100, branches: 100, functions: 100, statements: 100 },
    },
  },
});
