// U23–U26 (2026-10-23 UI/UX pass): the account form opens on demand and the row buttons sit in
// their rows; a fax row opens with a click anywhere and shows a short time; the fax page shows the
// details and the note beside the pages; a setting that needs a restart has a mark, explained once.

import { expect, test } from '@playwright/test';
import { csrf, ensureFax, PW, signedIn, tr } from './support';

test.describe.configure({ mode: 'serial' });

test('U23 the new account form opens with its button, focused, closes after creating; the row buttons sit inside their rows', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    await page.getByRole('link', { name: tr('nav.users'), exact: true }).click();
    await expect(page.getByRole('heading', { name: tr('users.title'), exact: true })).toBeVisible();
    await expect(page.locator('#n-user')).toHaveCount(0);
    await page.getByRole('button', { name: `+ ${tr('users.new')}`, exact: true }).click();
    await expect(page.locator('#n-user')).toBeFocused();
    // the form is above the list
    const formBox = (await page.locator('form.user-form').boundingBox())!;
    const tableBox = (await page.locator('table.users').boundingBox())!;
    expect(formBox.y).toBeLessThan(tableBox.y);

    await page.keyboard.type('u23user');
    await page.locator('#n-pw').fill(PW);
    await page.getByRole('button', { name: tr('users.create'), exact: true }).click();
    await expect(page.getByText(tr('users.created', { name: 'u23user' }))).toBeVisible();
    await expect(page.locator('#n-user')).toHaveCount(0);
    const row = page.locator('table.users tbody tr', { hasText: 'u23user' });
    await expect(row).toHaveCount(1);

    // every action button lies within the box of its own row (a flex <td> once broke this)
    for (const r of await page.locator('table.users tbody tr').all()) {
      const rb = (await r.boundingBox())!;
      for (const b of await r.getByRole('button').all()) {
        const bb = (await b.boundingBox())!;
        expect(bb.y).toBeGreaterThanOrEqual(rb.y - 0.5);
        expect(bb.y + bb.height).toBeLessThanOrEqual(rb.y + rb.height + 0.5);
      }
    }
    // editing opens its form, focused, also above the list
    await row.getByRole('button', { name: tr('users.edit', { name: 'u23user' }), exact: true }).click();
    await expect(page.locator('#e-dn')).toBeFocused();
    await expect(page.locator('#n-user')).toHaveCount(0);
  } finally {
    const r = await page.request.get('/api/v1/users');
    const u = ((await r.json()) as { users: { id: number; username: string }[] }).users.find((x) => x.username === 'u23user');
    if (u) await page.request.delete(`/api/v1/users/${u.id}`, { headers: { 'X-Sipfax-Csrf': await csrf(page.request) } });
    await context.close();
  }
});

test('U24 a click anywhere on a fax row opens it, the read button does not; the time is short with the full one as its title', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    const fax = await ensureFax(page.request, '0924000024');
    await page.goto('/#/faxes');
    const row = page.locator(`tr[data-fax-id="${fax.id}"]`);
    await expect(row).toBeVisible();
    // the read button toggles, and stays on the list
    const before = fax.is_read;
    await row.getByRole('button').click();
    await expect.poll(async () => ((await (await page.request.get(`/api/v1/faxes/${fax.id}`)).json()) as { is_read: boolean }).is_read).toBe(!before);
    await expect(page.getByRole('heading', { name: tr('faxes.title'), exact: true })).toBeVisible();
    // the time: today's fax says "today", the title has the seconds
    const link = row.getByRole('link');
    await expect(link).toHaveText(new RegExp(`^${tr('time.today')} \\d\\d:\\d\\d`));
    await expect(link).toHaveAttribute('title', /\d\d:\d\d:\d\d/);
    // a click on the number cell opens the fax
    await row.locator('td').nth(1).click();
    await expect(page.getByRole('heading', { name: tr('fax.title', { ani: '0924000024' }), exact: true })).toBeVisible();
  } finally {
    await context.close();
  }
});

test('U25 the fax page shows the details and the note beside the pages, without scrolling', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    const fax = await ensureFax(page.request, '0924000024');
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto(`/#/faxes/${fax.id}`);
    await expect(page.locator('.pageview img')).toBeVisible();
    const view = (await page.locator('.viewer').boundingBox())!;
    for (const sel of ['#info-title', '#note']) {
      const b = (await page.locator(sel).boundingBox())!;
      expect(b.x, `${sel} is right of the pages`).toBeGreaterThan(view.x + view.width - 1);
      expect(b.y + b.height, `${sel} is in the first screen`).toBeLessThan(800);
    }
    // the technical details wait behind "More details"
    await expect(page.getByText(fax.call_id, { exact: true })).toBeHidden();
    await page.getByText(tr('fax.more_details'), { exact: true }).click();
    await expect(page.getByText(fax.call_id, { exact: true })).toBeVisible();
    // on a narrow screen the details go below the pages
    await page.setViewportSize({ width: 800, height: 800 });
    const narrowView = (await page.locator('.viewer').boundingBox())!;
    expect((await page.locator('#info-title').boundingBox())!.y).toBeGreaterThan(narrowView.y + narrowView.height - 1);
  } finally {
    await context.close();
  }
});

test('U26 a field that needs a restart has a mark at its label; the sentence is said once at the top', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    await page.goto('/#/settings/sip');
    await expect(page.getByRole('heading', { name: tr('config.section.sip'), exact: true })).toBeVisible();
    await expect(page.getByText(tr('config.restart_legend'), { exact: true })).toBeVisible();
    // the long sentence is not shown under every field any more (it stays for screen readers)
    const sentence = page.getByText(tr('config.restart_field'), { exact: true });
    expect(await sentence.count()).toBeGreaterThan(1);
    for (const s of await sentence.all()) await expect(s).toHaveClass(/sr-only/);
    // the mark sits in the label without changing the field's name
    await expect(page.locator('label[for="f-bind"] .restart-mark')).toBeVisible();
    await expect(page.locator('#f-bind')).toHaveAccessibleName(tr('config.field.sip.bind'));
    await expect(page.locator('#f-bind')).toHaveAccessibleDescription(new RegExp(tr('config.restart_field')));
    // a field that applies at once has no mark
    await expect(page.locator('label[for="f-unknown_source"] .restart-mark')).toHaveCount(0);
  } finally {
    await context.close();
  }
});
