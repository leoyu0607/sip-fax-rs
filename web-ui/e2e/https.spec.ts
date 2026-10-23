// U19: the whole flow over HTTPS with a self-signed certificate (instance b of
// scripts/e2e-web-container.sh), and the session cookie is Secure.

import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { HTTPS_URL, HTTP_URL, PW, base, csrf, faxByAni, signOut, signedIn, sql, startFax, tr } from './support';

test('U19 over HTTPS: sign in, a fax arrives live, open it, download, note, sign out; the cookie is Secure', async ({ browser }) => {
  const ANI = '0919000019';
  const { context, page } = await signedIn(browser, 'amy', { instance: 'b' });
  const problems: string[] = [];
  page.on('console', (m) => m.type() === 'error' && problems.push(m.text()));
  page.on('pageerror', (e) => problems.push(String(e)));
  try {
    expect(page.url().startsWith(HTTPS_URL)).toBe(true);
    // the cookie
    const cookie = (await context.cookies()).find((c) => c.name === 'sipfax_session');
    expect(cookie, 'a session cookie').toBeDefined();
    expect(cookie).toMatchObject({ secure: true, httpOnly: true, sameSite: 'Strict' });
    // the security headers of the page itself
    const head = await page.request.get('/');
    expect(head.headers()['strict-transport-security']).toBe('max-age=31536000');
    expect(head.headers()['content-security-policy']).toContain("default-src 'self'");

    // the live link runs over wss: a fax shows up and finishes without a reload
    await page.goto('/#/faxes');
    await expect(page.getByText(tr('live.on'))).toBeAttached();
    const sending = startFax({ from: ANI, instance: 'b' });
    const row = page.locator('tr[data-fax-id]').filter({ hasText: ANI });
    await expect(row).toHaveCount(1, { timeout: 30_000 });
    await expect(row.locator('.badge')).toContainText(tr('fax.status.received'), { timeout: 60_000 });
    await sending.done;
    const fax = await faxByAni(page.request, ANI);
    expect(fax.pages).toBe(1);

    // open it: PNGs, PDF, read toggle, note
    await row.getByRole('link').click();
    await expect(page.getByRole('heading', { name: tr('fax.title', { ani: ANI }), exact: true })).toBeVisible();
    await expect
      .poll(() => page.locator('.scroller img').evaluate((i) => (i as HTMLImageElement).naturalWidth))
      .toBeGreaterThan(1000);
    await expect
      .poll(() => page.locator('.thumbs img').first().evaluate((i) => (i as HTMLImageElement).naturalWidth))
      .toBeGreaterThan(0);
    const [pdf] = await Promise.all([page.waitForEvent('download'), page.getByRole('link', { name: tr('fax.download_pdf') }).click()]);
    const bytes = readFileSync((await pdf.path())!);
    expect(bytes.subarray(0, 5).toString('latin1')).toBe('%PDF-');
    expect(bytes.length).toBe(fax.pdf_size);
    await page.getByRole('button', { name: tr('fax.mark_read'), exact: true }).click();
    await expect.poll(async () => (await faxByAni(page.request, ANI)).is_read).toBe(true);
    await page.locator('#note').fill('over https');
    await page.getByRole('button', { name: tr('fax.note_save'), exact: true }).click();
    await expect(page.getByText(tr('common.saved'), { exact: true })).toBeVisible();
    expect(await sql('b', 'select note n, read_changed_by r from faxes where id = ?', [fax.id])).toEqual([{ n: 'over https', r: 'amy' }]);

    // a state-changing call still needs the CSRF header over HTTPS
    const refused = await page.request.put(`/api/v1/faxes/${fax.id}/read`, { data: { read: false } });
    expect(refused.status()).toBe(403);
    expect((await faxByAni(page.request, ANI)).is_read).toBe(true);
    expect((await page.request.put(`/api/v1/faxes/${fax.id}/read`, { headers: { 'X-Sipfax-Csrf': await csrf(page.request) }, data: { read: false } })).status()).toBe(200);

    // sign out: the cookie is gone and the session is dead
    await signOut(page);
    await expect(page.getByRole('button', { name: tr('login.submit') })).toBeVisible();
    expect((await context.cookies()).find((c) => c.name === 'sipfax_session')).toBeUndefined();
    expect((await page.request.get('/api/v1/faxes')).status()).toBe(401);
    expect(problems, 'no console errors (CSP, mixed content, TLS)').toEqual([]);
  } finally {
    await context.close();
  }
});

test('U19 the same server over plain HTTP gives a cookie without Secure and no HSTS', async ({ browser }) => {
  expect(base('a')).toBe(HTTP_URL);
  const context = await browser.newContext({ baseURL: HTTP_URL });
  try {
    const r = await context.request.post('/api/v1/auth/login', { data: { username: 'amy', password: PW } });
    expect(r.status()).toBe(200);
    expect(r.headers()['strict-transport-security']).toBeUndefined();
    const set = r.headers()['set-cookie'] ?? '';
    expect(set).toContain('sipfax_session=');
    expect(set).toContain('HttpOnly');
    expect(set).toContain('SameSite=Strict');
    expect(set).not.toContain('Secure');
  } finally {
    await context.close();
  }
});
