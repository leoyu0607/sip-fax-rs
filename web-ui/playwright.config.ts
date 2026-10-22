import { defineConfig, devices } from '@playwright/test';

// The E2E tests (docs/test-plans/M4.md 6.2, 6.3) need the stack of scripts/e2e-web.sh: run that
// script (it builds, starts sip-faxd in a Docker container and then calls `playwright test`).
export default defineConfig({
  testDir: 'e2e',
  testMatch: '*.spec.ts',
  // one browser at a time: the cases share one server and change its settings
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 150_000,
  expect: { timeout: 15_000 },
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: process.env.E2E_HTTP_URL ?? 'http://127.0.0.1:18080',
    // the pages are checked in a known language and time zone; U17 changes the language
    locale: 'zh-TW',
    timezoneId: 'Asia/Taipei',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
});
