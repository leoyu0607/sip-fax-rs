// U10 (first sign-in), U14 (an agent has no settings or accounts), U18 (keyboard only).

import { expect, test, type Page } from '@playwright/test';
import { PW, csrf, faxByAni, login, sendFax, sql, tr } from './support';

test.describe.configure({ mode: 'serial' });

test('U10 first sign-in: the password change is forced, wrong input is refused, then the list opens', async ({ page }) => {
  const NEW = 'A-brand-new-secret-77';
  await page.goto('/');
  await page.getByLabel(tr('login.username')).fill('newbie');
  await page.getByLabel(tr('login.password')).fill(PW);
  await page.getByRole('button', { name: tr('login.submit') }).click();

  // the forced page: no menu, no list
  await expect(page.getByText(tr('password.forced'))).toBeVisible();
  await expect(page.getByRole('link', { name: tr('nav.faxes'), exact: true })).toHaveCount(0);
  // the server agrees: everything but the change is refused
  const refused = await page.request.get('/api/v1/faxes');
  expect(refused.status()).toBe(403);
  expect(((await refused.json()) as { error: { code: string } }).error.code).toBe('password_change_required');

  const current = page.getByLabel(tr('password.current'), { exact: true });
  const next = page.getByLabel(tr('password.new'), { exact: true });
  const again = page.getByLabel(tr('password.again'), { exact: true });
  const submit = page.getByRole('button', { name: tr('password.submit') });

  // too short: explained at once, nothing can be sent
  await current.fill(PW);
  await next.fill('short');
  await again.fill('short');
  await expect(page.getByRole('alert').filter({ hasText: tr('error.password_too_short') })).toBeVisible();
  await expect(submit).toBeDisabled();
  // the two entries differ
  await next.fill(NEW);
  await again.fill(`${NEW}x`);
  await expect(page.getByRole('alert').filter({ hasText: tr('password.mismatch') })).toBeVisible();
  await expect(submit).toBeDisabled();
  // the current password is wrong: the server refuses and the account still has to change it
  await current.fill('not-the-password-123');
  await again.fill(NEW);
  await submit.click();
  await expect(page.getByRole('alert').filter({ hasText: tr('error.wrong_password') })).toBeVisible();
  await expect(page.getByText(tr('password.forced'))).toBeVisible();
  expect((await sql('a', 'select must_change_password m from users where username = ?', ['newbie']))[0]?.m).toBe(1);

  // the right input
  await current.fill(PW);
  await submit.click();
  await expect(page.getByRole('heading', { name: tr('faxes.title'), exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: tr('nav.faxes'), exact: true })).toBeVisible();
  expect((await sql('a', 'select must_change_password m from users where username = ?', ['newbie']))[0]?.m).toBe(0);
  // the old password is gone, the new one works
  const old = await page.request.post('/api/v1/auth/login', { data: { username: 'newbie', password: PW } });
  expect(old.status()).toBe(401);
  const fresh = await page.request.post('/api/v1/auth/login', { data: { username: 'newbie', password: NEW } });
  expect(fresh.status()).toBe(200);
});

test('U14 an agent has no settings or accounts: no menu entry, a direct address is refused, the API says 403', async ({ page }) => {
  await login(page, 'amy');
  await expect(page.getByRole('link', { name: tr('nav.status'), exact: true })).toBeVisible();
  for (const n of ['nav.settings', 'nav.users', 'nav.audit']) {
    await expect(page.getByRole('link', { name: tr(n), exact: true })).toHaveCount(0);
  }

  const asked: string[] = [];
  page.on('request', (r) => asked.push(new URL(r.url()).pathname));
  for (const hash of ['#/settings', '#/settings/trunk:gw', '#/users', '#/audit']) {
    await page.goto(`/${hash}`);
    await page.reload(); // a fresh load of that address, not only a hash change
    await expect(page.getByRole('heading', { name: tr('error.forbidden'), exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: tr('config.title'), exact: true })).toHaveCount(0);
  }
  expect(asked.filter((p) => /\/api\/v1\/(config|users)/.test(p)), 'the page did not even ask').toEqual([]);

  // the API refuses too, and nothing changes
  const token = await csrf(page.request);
  const h = { 'X-Sipfax-Csrf': token };
  const usersBefore = await sql('a', 'select count(*) n from users');
  const sipBefore = await sql('a', "select version from config_sections where name = 'sip'");
  for (const path of ['/api/v1/config', '/api/v1/config/sip', '/api/v1/users', '/api/v1/config/audit']) {
    expect((await page.request.get(path)).status(), `GET ${path}`).toBe(403);
  }
  expect((await page.request.get('/api/v1/status')).status(), 'agents may read the status').toBe(200);
  expect((await page.request.put('/api/v1/config/sip', { headers: h, data: { version: 1, body: {} } })).status()).toBe(403);
  expect((await page.request.post('/api/v1/users', { headers: h, data: { username: 'evil', role: 'admin', password: PW } })).status()).toBe(403);
  expect((await page.request.put('/api/v1/users/1', { headers: h, data: { role: 'agent' } })).status()).toBe(403);
  expect((await page.request.delete('/api/v1/users/1', { headers: h })).status()).toBe(403);
  expect(await sql('a', 'select count(*) n from users')).toEqual(usersBefore);
  expect(await sql('a', "select version from config_sections where name = 'sip'")).toEqual(sipBefore);
  expect(await sql('a', "select count(*) n from users where username = 'evil'")).toEqual([{ n: 0 }]);
});

/** Presses Tab until the focused element satisfies `match`. */
async function tabTo(page: Page, what: string, match: (e: { tag: string; text: string; href: string | null }) => boolean, max = 80) {
  for (let i = 0; i < max; i++) {
    await page.keyboard.press('Tab');
    const e = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      return {
        tag: el?.tagName ?? '',
        text: (el?.getAttribute('aria-label') ?? el?.textContent ?? '').replace(/\s+/g, ' ').trim(),
        href: el?.getAttribute('href') ?? null,
      };
    });
    if (match(e)) return;
  }
  throw new Error(`Tab did not reach ${what}`);
}

test('U18 only Tab, Enter and typing: sign in, open a fax, toggle read and back', async ({ page }) => {
  const ANI = '0913000018';
  await sendFax({ from: ANI });
  await page.goto('/');
  // sign in
  await page.keyboard.press('Tab');
  await expect(page.locator('#username')).toBeFocused();
  await page.keyboard.type('amy');
  await page.keyboard.press('Tab');
  await expect(page.locator('#password')).toBeFocused();
  await page.keyboard.type(PW);
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: tr('faxes.title'), exact: true })).toBeVisible();

  const fax = await faxByAni(page.request, ANI);
  expect(fax.is_read).toBe(false);
  // open the fax: Tab to its link, Enter
  await tabTo(page, 'the link of the fax', (e) => e.href === `#/faxes/${fax.id}`);
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: tr('fax.title', { ani: ANI }), exact: true })).toBeVisible();
  // opening did not mark it read
  expect((await faxByAni(page.request, ANI)).is_read).toBe(false);

  // the read button
  await tabTo(page, 'the read button', (e) => e.tag === 'BUTTON' && e.text === tr('fax.mark_read'));
  await page.keyboard.press('Enter');
  await expect.poll(async () => (await faxByAni(page.request, ANI)).is_read).toBe(true);
  await expect(page.getByRole('button', { name: tr('fax.mark_unread'), exact: true })).toBeFocused();
  // and back with the same key, focus kept on the button
  await page.keyboard.press('Enter');
  await expect.poll(async () => (await faxByAni(page.request, ANI)).is_read).toBe(false);

  // the way back to the list is a link too
  await tabTo(page, 'the link back', (e) => e.href === '#/faxes' && e.text.includes(tr('fax.back')));
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: tr('faxes.title'), exact: true })).toBeVisible();
});
