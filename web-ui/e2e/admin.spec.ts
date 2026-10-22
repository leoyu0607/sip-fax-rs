// U08 (settings forms: field errors, conflict, secrets), U15 (a trunk's number settings apply to
// the next call), U16 (a setting that needs a restart says so), U20 (the uptime counts on).

import { expect, test, type Page } from '@playwright/test';
import { csrf, login, sendFax, signedIn, sql, tr } from './support';

test.describe.configure({ mode: 'serial' });

/** Opens a settings section through the menu, as a user would. */
async function openSection(page: Page, section: string) {
  await page.getByRole('link', { name: tr('nav.settings'), exact: true }).click();
  await page.getByRole('navigation', { name: tr('config.sections') }).getByRole('link', { name: tr(`config.section.${section}`), exact: true }).click();
  await expect(page.getByRole('heading', { name: tr(`config.section.${section}`), exact: true })).toBeVisible();
}

async function sectionRow(name: string) {
  const rows = await sql('a', 'select body, version from config_sections where name = ?', [name]);
  return rows[0] as { body: string; version: number } | undefined;
}

test('U08 a 422 shows its error at the route and delivery (routes[i].deliveries[j]) and stores nothing', async ({ page }) => {
  await login(page, 'root');
  await openSection(page, 'routes');
  const before = await sectionRow('routes');
  expect(before, 'the routes section exists').toBeDefined();

  // a route with a folder delivery whose directory is empty
  await page.getByRole('button', { name: `+ ${tr('config.add_route')}` }).click();
  await page.getByRole('button', { name: `+ ${tr('delivery.kind_value.folder')}` }).click();
  const answered = page.waitForResponse((r) => r.url().includes('/api/v1/config/routes') && r.request().method() === 'PUT');
  await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
  const res = await answered;
  expect(res.status()).toBe(422);
  const err = ((await res.json()) as { error: { code: string; fields: Record<string, { code: string }> } }).error;
  expect(err.code).toBe('invalid');
  // the keys are paths with their indices; the server judges a route and a delivery as a whole
  const route = 'routes[0]';
  const delivery = 'routes[0].deliveries[0]';
  expect(Object.keys(err.fields).sort()).toEqual([route, delivery]);

  // each message is inside its own card, in the page's language, from the code
  await expect(page.locator('#route-0-err')).toHaveText(tr(`validation.${err.fields[route]!.code}`));
  await expect(page.locator('#route-0-delivery-0-err')).toHaveText(tr(`validation.${err.fields[delivery]!.code}`));
  await expect(page.getByRole('alert').filter({ hasText: tr('config.invalid') })).toBeVisible();
  // nothing was stored
  expect(await sectionRow('routes')).toEqual(before);

  // fix it: the error goes away and the section is stored with the next version
  await page.locator('#f-routes_0__default').check();
  await page.locator('#f-routes_0__deliveries_0__dir').fill('/tmp/e2e/out');
  await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
  await expect(page.getByText(tr('config.saved'))).toBeVisible();
  await expect(page.locator('#route-0-err, #route-0-delivery-0-err')).toHaveCount(0);
  const after = await sectionRow('routes');
  expect(after!.version).toBe(before!.version + 1);
  expect(JSON.parse(after!.body).routes[0].deliveries[0].dir).toBe('/tmp/e2e/out');
  // leave the server as it was
  await page.getByRole('button', { name: tr('config.remove_route'), exact: true }).click();
  await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
  await expect(page.getByText(tr('config.saved'))).toBeVisible();
  expect(JSON.parse((await sectionRow('routes'))!.body).routes).toEqual([]);
});

test('U08 a 422 in a plain form shows the error at the field, by its code, and stores nothing', async ({ page }) => {
  await login(page, 'root');
  await openSection(page, 'media');
  const before = await sectionRow('media');
  await page.locator('#f-port_range').fill('31100-31000');
  const answered = page.waitForResponse((r) => r.url().includes('/api/v1/config/media') && r.request().method() === 'PUT');
  await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
  const res = await answered;
  expect(res.status()).toBe(422);
  const fields = ((await res.json()) as { error: { fields: Record<string, { code: string }> } }).error.fields;
  expect(Object.keys(fields)).toEqual(['port_range']);
  expect(fields.port_range!.code).toBe('reversed');
  await expect(page.locator('#f-port_range-err')).toHaveText(tr('validation.reversed'));
  await expect(page.locator('#f-port_range')).toHaveAttribute('aria-invalid', 'true');
  expect(await sectionRow('media')).toEqual(before);
});

test('U08 a 409 asks for a reload and does not overwrite the other change', async ({ browser }) => {
  const one = await signedIn(browser, 'root');
  const two = await signedIn(browser, 'root');
  try {
    for (const p of [one.page, two.page]) {
      await openSection(p, 'retention');
    }
    const days = (p: Page) => p.locator('#f-files_days');
    const start = await sectionRow('retention');
    // browser two saves first
    await days(two.page).fill('111');
    await two.page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    await expect(two.page.getByText(tr('config.saved'))).toBeVisible();
    const mid = await sectionRow('retention');
    expect(mid!.version).toBe(start!.version + 1);
    expect(JSON.parse(mid!.body).files_days).toBe(111);

    // browser one still has the old version
    await days(one.page).fill('222');
    const answered = one.page.waitForResponse((r) => r.url().includes('/api/v1/config/retention') && r.request().method() === 'PUT');
    await one.page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    expect((await answered).status()).toBe(409);
    const conflict = one.page.locator('.conflict');
    await expect(conflict).toContainText(tr('config.conflict'));
    expect(await sectionRow('retention'), 'the other change is untouched').toEqual(mid);

    // reload: the other value comes in, the warning goes
    await conflict.getByRole('button', { name: tr('config.reload'), exact: true }).click();
    await expect(days(one.page)).toHaveValue('111');
    await expect(conflict).toHaveCount(0);
    // and now saving works
    await days(one.page).fill('333');
    await one.page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    await expect(one.page.getByText(tr('config.saved'))).toBeVisible();
    expect(JSON.parse((await sectionRow('retention'))!.body).files_days).toBe(333);
    // put the original back
    await days(one.page).fill(String(JSON.parse(start!.body).files_days));
    await one.page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    await expect(one.page.getByText(tr('config.saved'))).toBeVisible();
  } finally {
    await one.context.close();
    await two.context.close();
  }
});

test('U08 a secret that is not typed again goes back as {"set": true, ref} and the stored ciphertext does not change', async ({ page }) => {
  page.on('dialog', (d) => void d.accept());
  await login(page, 'root');
  await openSection(page, 'trunks');
  await page.getByLabel(tr('config.new_trunk'), { exact: true }).fill('sec');
  await page.getByRole('button', { name: tr('common.add'), exact: true }).click();
  await expect(page.getByRole('heading', { name: tr('config.trunk_title', { name: 'sec' }) })).toBeVisible();

  const stored = async () => {
    const r = await sectionRow('trunk:sec');
    return r === undefined ? undefined : { version: r.version, body: JSON.parse(r.body) as { register?: { password?: string | null }; t38_policy?: string } };
  };
  const save = () => page.getByRole('button', { name: tr('common.save'), exact: true });
  const saved = () => expect(page.getByText(tr('config.saved'))).toBeVisible();
  const state = page.locator('.field').filter({ has: page.locator('#f-register_password') }).locator('p').first();
  const put = () => page.waitForRequest((r) => r.method() === 'PUT' && r.url().includes('/api/v1/config/trunk'));

  // 1. a new trunk with a secret typed in: stored encrypted, the plain text nowhere in the row
  await page.locator('#f-peer').fill('127.0.0.1:5090');
  await expect(state).toContainText(tr('config.secret_unset'));
  await page.locator('#f-register_password').fill('Sup3r-secret-value');
  await save().click();
  // the form stays with its message; the field now reads back as stored
  await saved();
  await expect(state).toContainText(tr('config.secret_set'));
  await expect(page.locator('#f-register_password')).toHaveValue('');
  await expect.poll(stored).toBeDefined();
  const first = await stored();
  const cipher1 = first!.body.register!.password!;
  expect(cipher1).toMatch(/^enc:v1:/);
  expect(JSON.stringify((await sectionRow('trunk:sec'))!.body)).not.toContain('Sup3r-secret-value');

  // 2. reload; change something else, leave the secret alone
  await page.reload();
  await expect(page.getByRole('heading', { name: tr('config.trunk_title', { name: 'sec' }) })).toBeVisible();
  await expect(state).toContainText(tr('config.secret_set'));
  await page.locator('#f-t38_policy').selectOption('wait-peer');
  const sent = put();
  await save().click();
  const body = (await sent).postDataJSON() as { version: number; body: { register: { password: unknown }; t38_policy: string } };
  expect(body.body.t38_policy).toBe('wait-peer');
  expect(body.body.register.password, 'the server gave {set, ref}; it goes back as it came').toMatchObject({ set: true });
  expect(typeof (body.body.register.password as { ref: unknown }).ref).not.toBe('undefined');
  await saved();
  const second = await stored();
  expect(second!.version).toBe(first!.version + 1);
  expect(second!.body.t38_policy).toBe('wait-peer');
  expect(second!.body.register!.password, 'the ciphertext is the very same').toBe(cipher1);

  // 3. a new value replaces it
  await page.locator('#f-register_password').fill('Another-secret-2');
  await save().click();
  await saved();
  const third = await stored();
  expect(third!.body.register!.password).toMatch(/^enc:v1:/);
  expect(third!.body.register!.password).not.toBe(cipher1);

  // 4. clear it
  await page.getByLabel(tr('config.secret_clear'), { exact: true }).check();
  await save().click();
  await saved();
  expect((await stored())!.body.register?.password ?? null).toBeNull();
  // the form shows the cleared state at once, and after a reload too
  await expect(state).toContainText(tr('config.secret_unset'));
  await page.reload();
  await expect(state).toContainText(tr('config.secret_unset'));

  // remove the trunk again (the delete button and its confirmation)
  await openSection(page, 'trunks');
  const item = page.locator('.trunk-list li').filter({ hasText: 'sec' });
  await item.getByRole('button', { name: tr('common.delete'), exact: true }).click();
  await expect(page.locator('.trunk-list li').filter({ hasText: 'sec' })).toHaveCount(0);
  expect(await sectionRow('trunk:sec')).toBeUndefined();
});

test('U15 the admin changes the number settings of a trunk: the next fax gets its ANI by the new setting, no restart', async ({ page }) => {
  const FROM = '+886912345678';
  await login(page, 'root');
  // start from the default (a repeated run on the same server), and look at new rows only
  const cur = (await (await page.request.get('/api/v1/config/trunk%3Agw')).json()) as { version: number; body: { numbers?: Record<string, unknown> } };
  if (cur.body.numbers?.ani_strip_prefix) {
    const body = { ...cur.body, numbers: { ...cur.body.numbers, ani_strip_prefix: null } };
    const r = await page.request.put('/api/v1/config/trunk%3Agw', { headers: { 'X-Sipfax-Csrf': await csrf(page.request) }, data: { version: cur.version, body } });
    expect(r.status()).toBe(200);
  }
  const lastId = ((await sql('a', 'select coalesce(max(id), 0) m from faxes'))[0] as { m: number }).m;
  // before: the number is stored as it came in the From header
  await sendFax({ from: FROM, to: '0287654321' });
  expect(await sql('a', 'select ani, dnis from faxes where id > ? order by id', [lastId])).toEqual([
    { ani: FROM, dnis: '0287654321' },
  ]);

  await page.getByRole('link', { name: tr('nav.settings'), exact: true }).click();
  await page.getByRole('navigation', { name: tr('config.sections') }).getByRole('link', { name: tr('config.section.trunks'), exact: true }).click();
  await page.locator('.trunk-list').getByRole('link', { name: 'gw', exact: true }).click();
  await expect(page.getByRole('heading', { name: tr('config.trunk_title', { name: 'gw' }) })).toBeVisible();
  const prefix = page.locator('#f-numbers_ani_strip_prefix');
  await expect(prefix).toHaveValue('');
  await prefix.fill('+886');
  await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
  await expect(page.getByText(tr('config.saved'))).toBeVisible();
  // nothing of it waits for a restart
  await expect(page.locator('.ok .restart')).toHaveCount(0);
  await expect(page.locator('.notice.restart')).toHaveCount(0);
  const audit = await sql('a', "select action a, changed_by b from config_audit where section = 'trunk:gw' order by id desc limit 1");
  expect(audit).toEqual([{ a: 'update', b: 'root' }]);

  // after: the same call, the same process
  await sendFax({ from: FROM, to: '0287654321' });
  expect(await sql('a', 'select ani from faxes where id > ? order by id', [lastId])).toEqual([
    { ani: FROM },
    { ani: '912345678' },
  ]);
  // the list was not open when it arrived: it waits behind the banner, then shows the new number
  await page.getByRole('link', { name: tr('nav.faxes'), exact: true }).click();
  await page.getByRole('button', { name: tr('faxes.new', { count: 1 }) }).click();
  await expect(page.locator('tr[data-fax-id] td:nth-child(2)').getByText('912345678', { exact: true }).first()).toBeVisible();
});

test('U16 a change of sip.bind is marked beforehand, says it needs a restart, and is not applied by itself', async ({ browser }) => {
  const main = await signedIn(browser, 'root');
  const watcher = await signedIn(browser, 'root');
  try {
    const { page } = main;
    await openSection(page, 'sip');
    const bind = page.locator('#f-bind');
    const original = await bind.inputValue();
    expect(original).toBe('127.0.0.1:5060');
    // marked before anything is changed; a field that applies at once is not
    await expect(page.locator('#f-bind-restart')).toHaveText(new RegExp(tr('config.restart_field')));
    await expect(page.locator('#f-unknown_source-restart')).toHaveCount(0);

    // another admin watches the status page
    await watcher.page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
    await expect(watcher.page.getByRole('heading', { name: tr('status.title'), exact: true })).toBeVisible();
    await expect(watcher.page.locator('.restart')).toHaveCount(0);

    await bind.fill('127.0.0.1:5099');
    await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    await expect(page.locator('.ok .restart')).toHaveText(new RegExp(tr('config.restart_required', { fields: 'sip.bind' })));
    await expect(page.locator('.notice.restart')).toContainText(tr('config.pending_restart', { fields: 'sip.bind' }));
    // the status page of the other admin learns it by itself (status.changed)
    await expect(watcher.page.locator('.restart')).toContainText(tr('config.pending_restart', { fields: 'sip.bind' }), { timeout: 20_000 });
    // stored, but not applied: the old address still takes calls
    expect(JSON.parse((await sectionRow('sip'))!.body).bind).toBe('127.0.0.1:5099');
    await sendFax({ from: '0916000016' });

    // back to the original: nothing waits for a restart any more
    await bind.fill(original);
    await page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    await expect(page.getByText(tr('config.saved'))).toBeVisible();
    await expect(page.locator('.notice.restart')).toHaveCount(0);
    await expect(watcher.page.locator('.restart')).toHaveCount(0, { timeout: 20_000 });
    expect(JSON.parse((await sectionRow('sip'))!.body).bind).toBe(original);
  } finally {
    await main.context.close();
    await watcher.context.close();
  }
});

test('U20 the uptime on the status page counts on every second without asking the server', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    await page.getByRole('link', { name: tr('nav.status'), exact: true }).click();
    const uptime = page.locator('#uptime');
    await expect(uptime).toHaveText(/^\d+(:\d\d){1,2}$/);
    let asked = 0;
    page.on('request', (r) => {
      if (new URL(r.url()).pathname.endsWith('/api/v1/status')) asked++;
    });
    const before = await uptime.textContent();
    await expect(uptime).not.toHaveText(before!, { timeout: 3000 });
    const after = await uptime.textContent();
    await expect(uptime).not.toHaveText(after!, { timeout: 3000 });
    expect(asked).toBe(0);
  } finally {
    await context.close();
  }
});
