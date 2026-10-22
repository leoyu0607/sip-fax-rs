// F05 (M4f): the "no trunk" banner of a new installation: the administrator and the agent each
// see their own text, in both languages; the administrator's has the link to Settings → Trunks;
// it goes away without a reload as soon as a trunk exists. Runs on instance c of
// scripts/e2e-web-container.sh (no trunk at all); a and b have trunks and show no banner.

import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test';
import { csrf, signedIn, sql, tr } from './support';
import { NOTRUNK_URL } from './support';

test.describe.configure({ mode: 'serial' });

type Loc = 'zh-TW' | 'en';

/** A signed-in browser of `user` on instance c in `locale` (the browser's language decides). */
async function open(browser: Browser, user: string, locale: Loc): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext({ baseURL: NOTRUNK_URL, locale, timezoneId: 'Asia/Taipei' });
  const page = await context.newPage();
  await page.goto('/');
  // the login form speaks the language of the browser too
  await page.getByLabel(tr('login.username', {}, locale)).fill(user);
  await page.getByLabel(tr('login.password', {}, locale)).fill('Correct-horse-battery-1');
  await page.getByRole('button', { name: tr('login.submit', {}, locale) }).click();
  await expect(page.locator('header.topbar')).toBeVisible();
  return { context, page };
}

const banner = (page: Page) => page.locator('.setup-banner');

async function trunkCount(): Promise<number> {
  const rows = await sql('c', "select count(*) n from config_sections where name like 'trunk:%'");
  return (rows[0] as { n: number }).n;
}

for (const locale of ['zh-TW', 'en'] as Loc[]) {
  test(`F05 the administrator sees the no-trunk text with the link to Settings → Trunks (${locale})`, async ({ browser }) => {
    expect(await trunkCount(), 'instance c starts without a trunk').toBe(0);
    const { context, page } = await open(browser, 'root', locale);
    try {
      await expect(banner(page)).toBeVisible();
      await expect(banner(page)).toContainText(tr('setup.no_trunk_admin', {}, locale));
      // not the agent's text
      await expect(banner(page)).not.toContainText(tr('setup.no_trunk_agent', {}, locale));
      const link = banner(page).getByRole('link', { name: tr('setup.go_trunks', {}, locale) });
      await expect(link).toHaveAttribute('href', '#/settings/trunks');
      await link.click();
      await expect(page).toHaveURL(/#\/settings\/trunks$/);
      await expect(page.getByRole('heading', { name: tr('config.section.trunks', {}, locale), exact: true })).toBeVisible();
      // the banner is on every page
      await expect(banner(page)).toBeVisible();
    } finally {
      await context.close();
    }
  });

  test(`F05 the agent sees the contact-your-administrator text and no link (${locale})`, async ({ browser }) => {
    const { context, page } = await open(browser, 'amy', locale);
    try {
      await expect(banner(page)).toBeVisible();
      await expect(banner(page)).toContainText(tr('setup.no_trunk_agent', {}, locale));
      await expect(banner(page)).not.toContainText(tr('setup.no_trunk_admin', {}, locale));
      await expect(banner(page).getByRole('link')).toHaveCount(0);
    } finally {
      await context.close();
    }
  });
}

test('F05 an instance that has a trunk shows no banner (control)', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    await expect(page.getByRole('heading', { name: tr('faxes.title') })).toBeVisible();
    await expect(banner(page)).toHaveCount(0);
  } finally {
    await context.close();
  }
});

test('F05 adding the first trunk removes the banner and deleting it brings it back, for the administrator and the agent, without a reload', async ({ browser }) => {
  const admin = await open(browser, 'root', 'en');
  const agent = await open(browser, 'amy', 'zh-TW');
  try {
    for (const p of [admin.page, agent.page]) {
      await expect(banner(p)).toBeVisible();
      // a marker that a page reload would erase
      await p.evaluate(() => ((window as unknown as { __noReload: number }).__noReload = 1));
    }
    // the administrator adds a trunk in Settings → Trunks, as the link says
    await banner(admin.page).getByRole('link', { name: tr('setup.go_trunks', {}, 'en') }).click();
    await admin.page.getByLabel(tr('config.new_trunk', {}, 'en'), { exact: true }).fill('first');
    await admin.page.getByRole('button', { name: tr('common.add', {}, 'en'), exact: true }).click();
    await expect(admin.page.getByRole('heading', { name: tr('config.trunk_title', { name: 'first' }, 'en') })).toBeVisible();
    await admin.page.locator('#f-peer').fill('127.0.0.1:5090');
    // nothing saved yet: the banner is still there, and so is the missing trunk
    await expect(banner(admin.page)).toBeVisible();
    expect(await trunkCount()).toBe(0);
    await admin.page.getByRole('button', { name: tr('common.save', {}, 'en'), exact: true }).click();
    await expect(admin.page.getByText(tr('config.saved', {}, 'en'))).toBeVisible();
    expect(await trunkCount()).toBe(1);

    // gone on both, and neither page was reloaded
    await expect(banner(admin.page)).toHaveCount(0);
    await expect(banner(agent.page)).toHaveCount(0);
    for (const p of [admin.page, agent.page]) {
      expect(await p.evaluate(() => (window as unknown as { __noReload?: number }).__noReload)).toBe(1);
    }
    // a page loaded now shows none either
    await agent.page.reload();
    await expect(agent.page.locator('header.topbar')).toBeVisible();
    await expect(banner(agent.page)).toHaveCount(0);
    await agent.page.evaluate(() => ((window as unknown as { __noReload: number }).__noReload = 1));

    // deleting the last trunk brings the banner back, again without a reload
    admin.page.on('dialog', (d) => void d.accept());
    await admin.page.getByRole('navigation', { name: tr('config.sections', {}, 'en') }).getByRole('link', { name: tr('config.section.trunks', {}, 'en'), exact: true }).click();
    const item = admin.page.locator('.trunk-list li').filter({ hasText: 'first' });
    await item.getByRole('button', { name: tr('common.delete', {}, 'en'), exact: true }).click();
    await expect(admin.page.locator('.trunk-list li').filter({ hasText: 'first' })).toHaveCount(0);
    expect(await trunkCount()).toBe(0);
    await expect(banner(admin.page)).toBeVisible();
    await expect(banner(agent.page)).toBeVisible();
    await expect(banner(agent.page)).toContainText(tr('setup.no_trunk_agent'));
    expect(await agent.page.evaluate(() => (window as unknown as { __noReload?: number }).__noReload)).toBe(1);
  } finally {
    // leave instance c as it was, whatever happened (a repeated run on the same server)
    const r = await admin.page.request.get('/api/v1/config/trunk%3Afirst');
    if (r.status() === 200) {
      const v = ((await r.json()) as { version: number }).version;
      await admin.page.request.delete(`/api/v1/config/trunk%3Afirst?version=${v}`, { headers: { 'X-Sipfax-Csrf': await csrf(admin.page.request) } });
    }
    await admin.context.close();
    await agent.context.close();
  }
});
