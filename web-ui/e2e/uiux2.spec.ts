// U27–U33 (2026-10-23, second UI/UX pass): the sign-in page, the live-updates notice, the empty
// fax list, the column widths, unsaved changes, the previous / next fax, today on the status page.

import { expect, test, type Page } from '@playwright/test';
import { ensureFax, HTTP_URL, NOTRUNK_URL, PW, signedIn, tr } from './support';

test.describe.configure({ mode: 'serial' });

test('U27 the sign-in page: no "please sign in" line, the password can be shown, the language sits apart below', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('button', { name: tr('login.submit') })).toBeVisible();
  await expect(page.getByText(tr('login.subtitle'), { exact: true })).toHaveCount(0);
  const pw = page.locator('#password');
  await pw.fill(PW);
  await expect(pw).toHaveAttribute('type', 'password');
  const reveal = page.getByRole('button', { name: tr('login.show_password'), exact: true });
  await expect(reveal).toHaveAttribute('aria-pressed', 'false');
  await reveal.click();
  await expect(pw).toHaveAttribute('type', 'text');
  await expect(page.getByRole('button', { name: tr('login.hide_password'), exact: true })).toHaveAttribute('aria-pressed', 'true');
  await page.getByRole('button', { name: tr('login.hide_password'), exact: true }).click();
  await expect(pw).toHaveAttribute('type', 'password');
  // the field keeps its name; the language is below the sign-in button
  await expect(pw).toHaveAccessibleName(tr('login.password'));
  const submit = (await page.getByRole('button', { name: tr('login.submit') }).boundingBox())!;
  expect((await page.locator('#login-locale').boundingBox())!.y).toBeGreaterThan(submit.y + submit.height);
});

test('U28 nothing shows while the live updates work; a notice appears when they have been down a few seconds', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await expect(page.getByText(tr('live.on'))).toBeAttached();
    await expect(page.locator('.live.off')).toHaveCount(0);
    await expect(page.getByText(tr('live.on'))).toHaveClass(/sr-only/);
  } finally {
    await context.close();
  }
  // a second browser whose event stream is always refused
  const context2 = await browser.newContext({ baseURL: HTTP_URL, locale: 'zh-TW' });
  const page2 = await context2.newPage();
  try {
    await page2.routeWebSocket(/\/api\/v1\/events/, (ws) => ws.close({ code: 1011 }));
    await page2.goto('/');
    await page2.locator('#username').fill('amy');
    await page2.locator('#password').fill(PW);
    await page2.getByRole('button', { name: tr('login.submit') }).click();
    await expect(page2.locator('header.topbar')).toBeVisible();
    // not at once (a short reconnect is not worth a notice) ...
    await expect(page2.locator('.live.off')).toHaveCount(0);
    // ... but after a few seconds
    await expect(page2.locator('.live.off')).toContainText(tr('live.off'), { timeout: 10_000 });
  } finally {
    await context2.close();
  }
});

test('U29 an empty list says whether there are no faxes at all or none that match, with the way out', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await ensureFax(page.request, '0929000029');
    await page.getByRole('button', { name: tr('faxes.filter.toggle'), exact: true }).click();
    await page.locator('#f-q').fill('no-such-fax-anywhere');
    await page.getByRole('button', { name: tr('faxes.filter.apply'), exact: true }).click();
    await expect(page.getByText(tr('faxes.empty'), { exact: true })).toBeVisible();
    await expect(page.getByText(tr('faxes.empty_none'), { exact: true })).toHaveCount(0);
    await page.locator('td.empty').getByRole('button', { name: tr('faxes.filter.clear'), exact: true }).click();
    await expect(page.locator('table.faxes tbody tr[data-fax-id]').first()).toBeVisible();
  } finally {
    await context.close();
  }
  // instance c has never received a fax
  const c = await browser.newContext({ baseURL: NOTRUNK_URL, locale: 'zh-TW' });
  const pc = await c.newPage();
  try {
    await pc.goto('/');
    await pc.locator('#username').fill('root');
    await pc.locator('#password').fill(PW);
    await pc.getByRole('button', { name: tr('login.submit') }).click();
    await expect(pc.getByText(tr('faxes.empty_none'), { exact: true })).toBeVisible();
    await expect(pc.locator('td.empty').getByRole('button')).toHaveCount(0);
  } finally {
    await c.close();
  }
});

test('U30 the fax list: the note takes the room that is left, the other columns are as wide as their content', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await ensureFax(page.request, '0929000029');
    await page.goto('/#/faxes');
    const row = page.locator('table.faxes tbody tr[data-fax-id]').first();
    await expect(row).toBeVisible();
    const cells = await row.locator('td').all();
    const widths = await Promise.all(cells.map(async (c) => (await c.boundingBox())!.width));
    const note = widths[6]!;
    for (const [i, w] of widths.entries()) if (i !== 6) expect(w, `column ${i}`).toBeLessThan(note);
    // nothing wraps
    const heights = await Promise.all(cells.map(async (c) => (await c.boundingBox())!.height));
    expect(Math.max(...heights) - Math.min(...heights)).toBeLessThan(2);
  } finally {
    await context.close();
  }
});

async function settingsSip(page: Page) {
  await page.goto('/#/settings/sip');
  await expect(page.getByRole('heading', { name: tr('config.section.sip'), exact: true })).toBeVisible();
}

test('U31 leaving a page with unsaved changes asks first; staying keeps them, leaving drops them', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    await settingsSip(page);
    const ua = page.locator('#f-user_agent');
    const before = await ua.inputValue();
    // nothing changed: no question
    await page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
    await expect(page.getByRole('heading', { name: tr('status.title'), exact: true })).toBeVisible();

    await settingsSip(page);
    await ua.fill('u31-unsaved');
    const asked: string[] = [];
    page.once('dialog', (d) => {
      asked.push(d.message());
      void d.dismiss();
    });
    await page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
    await expect.poll(() => asked).toEqual([tr('common.leave_unsaved')]);
    // stayed: the page, the address and the typed value
    await expect(page.getByRole('heading', { name: tr('config.section.sip'), exact: true })).toBeVisible();
    expect(new URL(page.url()).hash).toBe('#/settings/sip');
    await expect(ua).toHaveValue('u31-unsaved');

    // leaving after all
    page.once('dialog', (d) => void d.accept());
    await page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
    await expect(page.getByRole('heading', { name: tr('status.title'), exact: true })).toBeVisible();
    await settingsSip(page);
    await expect(page.locator('#f-user_agent')).toHaveValue(before);

    // closing the tab asks the browser's own question
    await page.locator('#f-user_agent').fill('u31-unsaved-2');
    const kinds: string[] = [];
    page.on('dialog', (d) => {
      kinds.push(d.type());
      void d.accept();
    });
    await page.close({ runBeforeUnload: true });
    await expect.poll(() => kinds).toContain('beforeunload');
  } finally {
    await context.close();
  }
});

test('U32 the previous / next fax of the list, from the fax page', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await ensureFax(page.request, '0932000031');
    await ensureFax(page.request, '0932000032');
    await page.goto('/#/faxes');
    const rows = page.locator('table.faxes tbody tr[data-fax-id]');
    await expect(rows.nth(1)).toBeVisible();
    const first = Number(await rows.nth(0).getAttribute('data-fax-id'));
    const second = Number(await rows.nth(1).getAttribute('data-fax-id'));
    await rows.nth(0).getByRole('link').click();
    const nav = page.getByRole('navigation', { name: tr('fax.nav') });
    // the newest has no previous one
    await expect(nav.getByRole('link', { name: `‹ ${tr('fax.newer')}` })).toHaveCount(0);
    await nav.getByRole('link', { name: `${tr('fax.older')} ›` }).click();
    expect(new URL(page.url()).hash).toBe(`#/faxes/${second}`);
    await nav.getByRole('link', { name: `‹ ${tr('fax.newer')}` }).click();
    expect(new URL(page.url()).hash).toBe(`#/faxes/${first}`);
    // opened by its address, without the list: no previous / next
    const fresh = await context.newPage();
    await fresh.goto(`/#/faxes/${second}`);
    await expect(fresh.getByRole('heading', { level: 1 })).toBeVisible();
    await expect(fresh.getByRole('navigation', { name: tr('fax.nav') }).getByRole('link')).toHaveCount(1);
  } finally {
    await context.close();
  }
});

test('U33 the status page shows today at a glance and the free disk space', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await ensureFax(page.request, '0929000029');
    await page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
    const d = new Date();
    const midnight = new Date(d.getFullYear(), d.getMonth(), d.getDate()).toISOString();
    const counts = (await (await page.request.get(`/api/v1/faxes/counts?since=${encodeURIComponent(midnight)}`)).json()) as {
      unread: number;
      since: number;
      deliveries: { failed: number; giving_up: number };
    };
    expect(counts.since).toBeGreaterThan(0);
    await expect(page.locator('#stat-today')).toHaveText(String(counts.since));
    await expect(page.locator('#stat-unread')).toHaveText(String(counts.unread));
    await expect(page.locator('#stat-failed')).toHaveText(String(counts.deliveries.failed));
    await expect(page.locator('#stat-giving-up')).toHaveText(String(counts.deliveries.giving_up));
    // the disk of the data directory (the server runs on Linux)
    await expect(page.locator('#disk-free')).toContainText('%');
    await expect(page.locator('meter.disk')).toBeVisible();
  } finally {
    await context.close();
  }
});
