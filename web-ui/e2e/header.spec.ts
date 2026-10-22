// U22: the header — the unread badge of "Faxes" follows the faxes live, and the account menu holds
// the account page, the language, the theme and signing out.

import { expect, test, type Page } from '@playwright/test';
import { csrf, faxByAni, sendFax, signedIn, tr, userMenu } from './support';

test.describe.configure({ mode: 'serial' });

async function apiUnread(page: Page): Promise<number> {
  const r = await page.request.get('/api/v1/faxes/counts');
  expect(r.status()).toBe(200);
  return ((await r.json()) as { unread: number }).unread;
}

/** The number on the badge, 0 when there is none. */
async function badge(page: Page): Promise<number> {
  const b = page.locator('.topbar nav .count');
  return (await b.count()) === 0 ? 0 : Number(await b.textContent());
}

test('U22 the unread badge shows the count and follows a new fax and the read flag without a reload', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await page.evaluate(() => ((window as unknown as { marker: number }).marker = 22));
    const before = await apiUnread(page);
    await expect.poll(() => badge(page)).toBe(before);
    // the link keeps its name; the count is its description
    const link = page.getByRole('link', { name: tr('nav.faxes'), exact: true });
    if (before > 0) await expect(link).toHaveAccessibleDescription(tr('nav.unread', { count: before }));

    const ANI = '0922000022';
    await sendFax({ from: ANI });
    await expect.poll(() => badge(page), { timeout: 20_000 }).toBe(before + 1);
    await expect(link).toHaveAccessibleDescription(tr('nav.unread', { count: before + 1 }));

    // read on the server (as another session would): the badge goes down
    const fax = await faxByAni(page.request, ANI);
    const token = await csrf(page.request);
    const put = (read: boolean) => page.request.put(`/api/v1/faxes/${fax.id}/read`, { headers: { 'X-Sipfax-Csrf': token }, data: { read } });
    expect((await put(true)).status()).toBe(200);
    await expect.poll(() => badge(page)).toBe(before);
    expect((await put(false)).status()).toBe(200);
    await expect.poll(() => badge(page)).toBe(before + 1);
    // all without a reload
    expect(await page.evaluate(() => (window as unknown as { marker?: number }).marker)).toBe(22);
  } finally {
    await context.close();
  }
});

test('U22 the account menu opens, closes with Escape and a click outside, and leads to the account page', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    const menu = page.locator('details.user-menu');
    const panel = menu.locator('.menu-panel');
    await expect(panel).toBeHidden();
    await expect(menu.locator('summary')).toHaveAccessibleName(tr('nav.menu_of', { name: 'amy' }));

    await userMenu(page);
    for (const label of [tr('nav.language'), tr('nav.theme')]) await expect(panel.getByLabel(label, { exact: true })).toBeVisible();
    await expect(panel.getByRole('button', { name: tr('nav.logout'), exact: true })).toBeVisible();
    // Escape closes it and gives the focus back to the menu button
    await page.keyboard.press('Escape');
    await expect(panel).toBeHidden();
    await expect(menu.locator('summary')).toBeFocused();
    // a click outside closes it
    await userMenu(page);
    await page.getByRole('heading', { name: tr('faxes.title'), exact: true }).click();
    await expect(panel).toBeHidden();
    // a choice in it changes the page and closes it
    await userMenu(page);
    await panel.getByRole('link', { name: tr('account.title'), exact: true }).click();
    await expect(page.getByRole('heading', { name: tr('account.title'), exact: true })).toBeVisible();
    await expect(panel).toBeHidden();
  } finally {
    await context.close();
  }
});
