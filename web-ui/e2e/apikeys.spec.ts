// M6a (docs/test-plans/M6a.md section 6): the "API keys" page. U01 (create a read-only key: shown
// once, copyable, then gone), U02 (revoke), U03 (an agent has no menu entry and no data),
// U04 (every text in both languages).

import { expect, test, type Page } from '@playwright/test';
import { HTTP_URL, PW, allTexts, csrf, signedIn, sql, tr, userMenu } from './support';

test.describe.configure({ mode: 'serial' });

const KEY_RE = /^sfx_[A-Za-z0-9_-]{43}$/;

/** The row of the key `name` in the list. */
function row(page: Page, name: string) {
  return page.locator('table.users tbody tr').filter({ has: page.getByRole('cell', { name, exact: true }) });
}

/** Opens the page and the form for a new key. */
async function openForm(page: Page) {
  await page.goto('/#/api-keys');
  await expect(page.getByRole('heading', { name: tr('apikeys.title'), exact: true })).toBeVisible();
  await page.getByRole('button', { name: `+ ${tr('apikeys.new')}` }).click();
  await expect(page.locator('#k-name')).toBeFocused();
}

/** Calls the fax API with a key (no cookies: a request of another system). */
async function withKey(key: string, path: string) {
  const r = await fetch(`${HTTP_URL}/api/v1${path}`, { headers: { Authorization: `Bearer ${key}` } });
  return { status: r.status, body: await r.text() };
}

let readKey = '';

test('U01 the administrator creates a read-only key: shown once, copyable, gone after closing; the list says what it is', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: HTTP_URL });
  try {
    // a menu entry for the administrator
    await expect(page.getByRole('link', { name: tr('nav.apikeys'), exact: true })).toBeVisible();
    await page.getByRole('link', { name: tr('nav.apikeys'), exact: true }).click();
    await expect(page.getByRole('heading', { name: tr('apikeys.title'), exact: true })).toBeVisible();
    await expect(page.getByText(tr('apikeys.none'))).toBeVisible();

    // the form: the name is checked by the server (the interface shows its refusal), the rights default to read only
    await page.getByRole('button', { name: `+ ${tr('apikeys.new')}` }).click();
    await expect(page.getByLabel(tr('apikeys.write'))).not.toBeChecked();
    await page.locator('#k-name').fill('Bad Name');
    await page.getByRole('button', { name: tr('apikeys.create'), exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: /\S/ })).toBeVisible();
    expect(await sql('a', 'select count(*) n from api_keys')).toEqual([{ n: 0 }]);

    // the right name
    await page.locator('#k-name').fill('e2e-reader');
    await page.getByRole('button', { name: tr('apikeys.create'), exact: true }).click();
    const shown = page.locator('#k-created');
    await expect(page.getByRole('heading', { name: tr('apikeys.created', { name: 'e2e-reader' }), exact: true })).toBeVisible();
    await expect(page.getByText(tr('apikeys.created_once'))).toBeVisible();
    readKey = await shown.inputValue();
    expect(readKey).toMatch(KEY_RE);

    // copy: the clipboard has exactly the key and the button says so
    await page.getByRole('button', { name: tr('apikeys.copy'), exact: true }).click();
    await expect(page.getByRole('button', { name: tr('apikeys.copied'), exact: true })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(readKey);

    // the list already has the row: name, the start of the key, the rights, active, no expiry, not used
    const r = row(page, 'e2e-reader');
    await expect(r).toHaveCount(1);
    await expect(r).toContainText(`${readKey.slice(0, 12)}…`);
    await expect(r).toContainText(tr('apikeys.scope_read'));
    await expect(r).toContainText(tr('apikeys.active'));
    await expect(r).toContainText(tr('apikeys.never'));
    await expect(r).toContainText('root');
    await expect(r.getByRole('cell').nth(5)).toHaveText('—');
    // the rest of the key is nowhere in the list
    expect(await page.locator('table.users').innerText()).not.toContain(readKey.slice(12));

    // closing the panel: the key is gone from the page, and a reload does not bring it back
    await page.getByRole('button', { name: tr('apikeys.done'), exact: true }).click();
    await expect(shown).toHaveCount(0);
    for (const reload of [false, true]) {
      if (reload) {
        await page.reload();
        await expect(page.getByRole('heading', { name: tr('apikeys.title'), exact: true })).toBeVisible();
        await expect(row(page, 'e2e-reader')).toHaveCount(1);
      }
      const html = await page.content();
      expect(html, 'the key is not in the page').not.toContain(readKey.slice(12));
      expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(readKey); // (the clipboard is the person's own)
    }
    // the browser stored it nowhere
    const stored = await page.evaluate(() => JSON.stringify([{ ...localStorage }, { ...sessionStorage }]));
    expect(stored).not.toContain(readKey.slice(12));

    // the server: the hash and the start only
    const rows = await sql('a', 'select name, prefix, scopes, length(token_hash) h, created_by c, expires_at e from api_keys');
    expect(rows).toEqual([{ name: 'e2e-reader', prefix: readKey.slice(0, 12), scopes: 'fax:read', h: 64, c: 'root', e: null }]);
    expect(await sql('a', 'select count(*) n from api_keys where token_hash = ? or prefix = ?', [readKey, readKey])).toEqual([{ n: 0 }]);

    // it works as a key, read only; and the list shows when and from where
    const ok = await withKey(readKey, '/faxes?limit=1');
    expect(ok.status).toBe(200);
    const put = await fetch(`${HTTP_URL}/api/v1/faxes/1/read`, {
      method: 'PUT',
      headers: { Authorization: `Bearer ${readKey}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ read: true }),
    });
    expect(put.status).toBe(403);
    await page.reload();
    await expect(row(page, 'e2e-reader').getByRole('cell').nth(5)).toContainText(/\(\d+\.\d+\.\d+\.\d+\)/); // (the address the server saw: the Docker gateway, here)

    // a second key: with write rights and an expiry day
    await openForm(page);
    await page.locator('#k-name').fill('e2e-writer');
    await page.getByLabel(tr('apikeys.write')).check();
    await page.locator('#k-exp').fill('2099-12-31');
    await page.getByRole('button', { name: tr('apikeys.create'), exact: true }).click();
    const writeKey = await page.locator('#k-created').inputValue();
    expect(writeKey).toMatch(KEY_RE);
    expect(writeKey).not.toBe(readKey);
    const w = row(page, 'e2e-writer');
    await expect(w).toContainText(tr('apikeys.scope_write'));
    await expect(w.getByRole('cell').nth(4)).not.toHaveText(tr('apikeys.never'));
    // the last valid day, as it was entered (the key ends at the start of the next day)
    await expect(w.getByRole('cell').nth(4)).toContainText('2099/12/31');
    await expect(w.getByRole('cell').nth(4)).not.toContainText('2100');
    const e = await sql('a', "select scopes s, expires_at e from api_keys where name = 'e2e-writer'");
    expect(e[0]?.s).toBe('fax:read,fax:write');
    expect(String(e[0]?.e)).toMatch(/^(2099-12-3[01]|2100-01-01)T/);
    // a name that exists is refused and says so; the first panel is replaced by the new one only on success
    await page.getByRole('button', { name: tr('apikeys.done'), exact: true }).click();
    await openForm(page);
    await page.locator('#k-name').fill('e2e-writer');
    await page.getByRole('button', { name: tr('apikeys.create'), exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: /\S/ })).toBeVisible();
    await expect(page.locator('#k-created')).toHaveCount(0);
    expect(await sql('a', 'select count(*) n from api_keys')).toEqual([{ n: 2 }]);
  } finally {
    await context.close();
  }
});

test('U02 revoking asks first, then marks the key revoked and the key stops working at once', async ({ browser }) => {
  expect(readKey, 'U01 made the key').toMatch(KEY_RE);
  const { context, page } = await signedIn(browser, 'root');
  try {
    await page.goto('/#/api-keys');
    const r = row(page, 'e2e-reader');
    await expect(r).toContainText(tr('apikeys.active'));

    // "cancel" in the confirmation: nothing happens
    let asked = '';
    page.once('dialog', (d) => {
      asked = d.message();
      void d.dismiss();
    });
    await r.getByRole('button', { name: tr('apikeys.revoke_of', { name: 'e2e-reader' }), exact: true }).click();
    await expect.poll(() => asked).toBe(tr('apikeys.revoke_confirm', { name: 'e2e-reader' }));
    await expect(r).toContainText(tr('apikeys.active'));
    expect((await withKey(readKey, '/faxes?limit=1')).status).toBe(200);
    expect(await sql('a', "select revoked_at r from api_keys where name = 'e2e-reader'")).toEqual([{ r: null }]);

    // "ok": revoked in the list, no button any more, the key is refused with the fixed error
    page.once('dialog', (d) => void d.accept());
    await r.getByRole('button', { name: tr('apikeys.revoke_of', { name: 'e2e-reader' }), exact: true }).click();
    await expect(r).toContainText(tr('apikeys.revoked'));
    await expect(r.getByRole('button')).toHaveCount(0);
    const refused = await withKey(readKey, '/faxes?limit=1');
    expect(refused.status).toBe(401);
    expect((JSON.parse(refused.body) as { error: { code: string } }).error.code).toBe('invalid_api_key');
    const rows = await sql('a', "select revoked_by b, revoked_at is not null r from api_keys where name = 'e2e-reader'");
    expect(rows).toEqual([{ b: 'root', r: 1 }]);
    // the other key is untouched and a reload keeps the picture
    await page.reload();
    await expect(row(page, 'e2e-reader')).toContainText(tr('apikeys.revoked'));
    await expect(row(page, 'e2e-writer')).toContainText(tr('apikeys.active'));
    // the name of a revoked key cannot be used again
    await openForm(page);
    await page.locator('#k-name').fill('e2e-reader');
    await page.getByRole('button', { name: tr('apikeys.create'), exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: /\S/ })).toBeVisible();
    expect(await sql('a', 'select count(*) n from api_keys')).toEqual([{ n: 2 }]);
  } finally {
    await context.close();
  }
});

test('U03 an agent has no menu entry for API keys, a direct address shows nothing, and the API refuses', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'amy');
  try {
    await expect(page.getByRole('link', { name: tr('nav.apikeys'), exact: true })).toHaveCount(0);
    const asked: string[] = [];
    page.on('request', (r) => asked.push(new URL(r.url()).pathname));
    await page.goto('/#/api-keys');
    await page.reload();
    await expect(page.getByRole('heading', { name: tr('error.forbidden'), exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: tr('apikeys.title'), exact: true })).toHaveCount(0);
    const html = await page.content();
    for (const name of ['e2e-reader', 'e2e-writer']) expect(html).not.toContain(name);
    expect(asked.filter((p) => p.includes('/api-keys')), 'the page did not even ask').toEqual([]);

    // the API refuses the three calls and nothing changes
    const token = await csrf(page.request);
    const h = { 'X-Sipfax-Csrf': token };
    expect((await page.request.get('/api/v1/api-keys')).status()).toBe(403);
    expect((await page.request.post('/api/v1/api-keys', { headers: h, data: { name: 'agent-made' } })).status()).toBe(403);
    expect((await page.request.post('/api/v1/api-keys/2/revoke', { headers: h, data: {} })).status()).toBe(403);
    expect(await sql('a', "select count(*) n from api_keys where name = 'agent-made'")).toEqual([{ n: 0 }]);
    expect(await sql('a', "select name from api_keys where revoked_at is null")).toEqual([{ name: 'e2e-writer' }]);
  } finally {
    await context.close();
  }
});

/** Everything a person can read on the page: text, labels, placeholders. */
async function readable(page: Page): Promise<string> {
  return page.evaluate(() => {
    const parts = [document.title, document.body.innerText];
    document.querySelectorAll('[aria-label],[placeholder],[title]').forEach((e) => {
      for (const a of ['aria-label', 'placeholder', 'title']) {
        const v = e.getAttribute(a);
        if (v) parts.push(v);
      }
    });
    return parts.join('\n');
  });
}

test('U04 every text of the page is there in both languages, and no text of the other language is', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    const mine = allTexts().filter(([k]) => k.startsWith('apikeys.') || k === 'nav.apikeys');
    expect(mine.length).toBeGreaterThan(25);
    // every text has a translation of its own (the same string in both would be an untranslated one)
    for (const [key, zh, en] of mine) {
      expect(zh.length, key).toBeGreaterThan(0);
      expect(en.length, key).toBeGreaterThan(0);
      expect(zh, `${key} is translated`).not.toBe(en);
    }
    // a key to look at, then the page in each language with the form and the created-key panel open
    await page.goto('/#/api-keys');
    for (const loc of ['zh-TW', 'en'] as const) {
      await userMenu(page);
      await page.locator('#locale').selectOption(loc);
      await expect(page.getByRole('link', { name: tr('nav.faxes', {}, loc), exact: true })).toBeVisible();
      await page.goto('/#/api-keys');
      await expect(page.getByRole('heading', { name: tr('apikeys.title', {}, loc), exact: true })).toBeVisible();
      await page.getByRole('button', { name: `+ ${tr('apikeys.new', {}, loc)}` }).click();
      // the form
      let text = await readable(page);
      for (const k of ['apikeys.new', 'apikeys.name', 'apikeys.name_help', 'apikeys.write', 'apikeys.write_help', 'apikeys.expires', 'apikeys.expires_help', 'apikeys.create']) {
        expect(text, `${loc}: ${k}`).toContain(tr(k, {}, loc));
      }
      // a created key panel
      const name = `lang-${loc.toLowerCase()}`;
      await page.locator('#k-name').fill(name);
      await page.getByRole('button', { name: tr('apikeys.create', {}, loc), exact: true }).click();
      await expect(page.getByRole('heading', { name: tr('apikeys.created', { name }, loc), exact: true })).toBeVisible();
      await page.getByRole('button', { name: tr('apikeys.copy', {}, loc), exact: true }).click();
      text = await readable(page);
      // the list, the panel and the table headers
      for (const k of ['apikeys.title', 'apikeys.intro', 'apikeys.created_once', 'apikeys.key', 'apikeys.done', 'apikeys.prefix', 'apikeys.scopes', 'apikeys.state', 'apikeys.last_used', 'apikeys.created_by', 'apikeys.scope_read', 'apikeys.active', 'apikeys.revoked', 'apikeys.never', 'apikeys.revoke']) {
        if (k === 'apikeys.revoke') continue; // the button text is the "Revoke {name}" label
        expect(text, `${loc}: ${k}`).toContain(tr(k, {}, loc));
      }
      expect(text).toContain(tr('apikeys.revoke_of', { name }, loc));
      // no text of the other language
      const other = loc === 'en' ? 'zh-TW' : 'en';
      if (loc === 'en') {
        expect(text.replace('繁體中文', '').match(/[\u3000-\u303f\u4e00-\u9fff\uff00-\uffef]+/g), 'no CJK text in English').toBeNull();
      } else {
        for (const [k, , en] of mine) {
          const piece = en.split(/\{\w+\}/).reduce((a, b) => (b.trim().length > a.trim().length ? b : a), '').trim();
          if (piece.length < 8 || !/[A-Za-z]{4}/.test(piece)) continue;
          expect(text, `zh-TW page shows the English ${k}`).not.toContain(piece);
        }
      }
      void other;
      // the confirmation of a revocation is in the language too
      let asked = '';
      page.once('dialog', (d) => {
        asked = d.message();
        void d.dismiss();
      });
      await page.getByRole('button', { name: tr('apikeys.revoke_of', { name }, loc), exact: true }).click();
      await expect.poll(() => asked).toBe(tr('apikeys.revoke_confirm', { name }, loc));
    }
    // back to the default language for the specs that follow
    await userMenu(page);
    await page.locator('#locale').selectOption('zh-TW');
    await expect(page.getByRole('link', { name: tr('nav.faxes'), exact: true })).toBeVisible();
    void PW;
  } finally {
    await context.close();
  }
});
