// Screenshots of docs/manual/agent.md and it.md, taken from a real server with real faxes (not a
// test: run with `scripts/e2e-web.sh --config playwright.shots.config.ts`).

import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';
import { PW, login, sendFax, signedIn, tr } from './support';

const OUT = fileURLToPath(new URL('../../docs/manual/images/', import.meta.url));
const shot = (page: Page, name: string) => page.screenshot({ path: `${OUT}${name}.png` });

test.describe.configure({ mode: 'serial' });

test('login and the first password change', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('button', { name: tr('login.submit') })).toBeVisible();
  await shot(page, 'login');
  // (not login(): this account lands on the password change, not on the menu)
  await page.getByLabel(tr('login.username')).fill('newbie');
  await page.getByLabel(tr('login.password')).fill(PW);
  await page.getByRole('button', { name: tr('login.submit') }).click();
  await expect(page.getByRole('heading', { name: tr('password.title') })).toBeVisible();
  await page.locator('#pw-current').fill('x');
  await shot(page, 'first-password');
});

test('faxes: list, filter, a fax, its note, a note conflict, new faxes', async ({ page, browser }) => {
  // a few real faxes, the newest last
  await sendFax({ from: '0912345678', to: '025570002', ident: 'HQ Sales' });
  await sendFax({ from: '0227001234', to: '025570001', ident: 'ACME Taipei', pages: 3 });
  await sendFax({ from: '0425556789', to: '025570001', ident: 'Taichung Office' });

  await login(page, 'amy');
  const rows = page.locator('tr[data-fax-id]');
  await expect(rows).toHaveCount(3);
  // one read, one with a note: the list shows both kinds
  await rows.filter({ hasText: '0912345678' }).getByRole('button').click();
  await rows.filter({ hasText: '0227001234' }).getByRole('link').click();
  await expect(page.locator('.thumbs img')).toHaveCount(3);
  await expect.poll(() => page.locator('.scroller img').evaluate((i) => (i as HTMLImageElement).naturalWidth)).toBeGreaterThan(0);
  await page.locator('#note').fill('已轉業務部，請 Wang 先生回電');
  await page.getByRole('button', { name: tr('fax.note_save') }).click();
  await expect(page.getByText(tr('common.saved'))).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot(page, 'detail');
  await page.locator('section[aria-labelledby="note-title"]').screenshot({ path: `${OUT}note.png` });

  // the conflict: ben types while amy saves another text
  const ben = await signedIn(browser, 'ben');
  await ben.page.goto(page.url());
  await ben.page.locator('#note').fill('客戶要求明天上午回覆');
  await page.locator('#note').fill('已轉業務部，Wang 先生已回電');
  await page.getByRole('button', { name: tr('fax.note_save') }).click();
  await expect(page.getByText(tr('common.saved'))).toBeVisible();
  await ben.page.getByRole('button', { name: tr('fax.note_save') }).click();
  await expect(ben.page.getByText(tr('fax.note_take_theirs'))).toBeVisible();
  await ben.page.locator('section[aria-labelledby="note-title"]').screenshot({ path: `${OUT}note-conflict.png` });
  await ben.context.close();

  await page.getByRole('link', { name: tr('fax.back') }).click();
  await expect(rows).toHaveCount(3);
  await shot(page, 'list');

  await page.getByRole('button', { name: tr('faxes.filter.toggle'), exact: true }).click();
  await page.locator('#f-q').fill('業務');
  await page.getByRole('button', { name: tr('faxes.filter.apply') }).click();
  await expect(rows).toHaveCount(1);
  await shot(page, 'filter');
  await page.getByRole('button', { name: tr('faxes.filter.clear') }).click();
  await page.getByRole('button', { name: tr('faxes.filter.toggle'), exact: true }).click();

  // a fax arrives while another page is open: the list holds it back behind the banner
  await page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
  await sendFax({ from: '0287651234', to: '025570001', ident: 'Kaohsiung' });
  await page.getByRole('link', { name: tr('nav.faxes'), exact: true }).click();
  await expect(page.getByRole('button', { name: tr('faxes.new', { count: 1 }) })).toBeVisible();
  await shot(page, 'new-faxes');
});

test('administration: settings, accounts, status', async ({ page }) => {
  await login(page, 'root');
  await page.goto('/#/settings/trunk:gw');
  await expect(page.locator('#f-peer')).toBeVisible();
  await shot(page, 'settings-trunk');
  await page.goto('/#/settings/sip');
  await expect(page.locator('#f-bind')).toBeVisible();
  await shot(page, 'settings-sip');
  await page.goto('/#/users');
  await expect(page.locator('tbody tr').first()).toBeVisible();
  await shot(page, 'users');
  await page.goto('/#/status');
  await expect(page.getByRole('heading', { name: tr('status.service') })).toBeVisible();
  await shot(page, 'status');
});
