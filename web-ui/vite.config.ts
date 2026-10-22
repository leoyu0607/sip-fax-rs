import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `npm run dev` proxies the API to a local sip-faxd (SIPFAX_DEV_API, default https://127.0.0.1:8443).
const api = process.env.SIPFAX_DEV_API ?? 'https://127.0.0.1:8443';

export default defineConfig({
  plugins: [svelte()],
  build: {
    // no inline script: the server's CSP is `default-src 'self'`
    modulePreload: { polyfill: false },
    assetsInlineLimit: 0,
    target: 'es2022',
  },
  server: {
    proxy: {
      '/api': { target: api, secure: false, ws: true, changeOrigin: false },
    },
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts'],
    coverage: {
      provider: 'v8',
      include: ['src/lib/**/*.ts'],
      thresholds: { statements: 70 },
    },
  },
});
