import { defineConfig } from '@playwright/test';
import base from './playwright.config';

// The screenshots of the manuals (docs/manual/images): the stack of scripts/e2e-web.sh with
//   scripts/e2e-web.sh --config playwright.shots.config.ts
export default defineConfig({
  ...base,
  testMatch: '*.shots.ts',
  reporter: [['list']],
  use: { ...base.use, viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 },
});
