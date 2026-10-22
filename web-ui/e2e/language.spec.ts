// U17: the language switch changes every visible text.

import { expect, test, type Page } from '@playwright/test';
import { allTexts, ensureFax, login, signedIn, sql, tr } from './support';

test.describe.configure({ mode: 'serial' });

type Loc = 'zh-TW' | 'en';

/** Everything a person can read on the page: text, labels, placeholders, option texts. */
async function readable(page: Page): Promise<string> {
  return page.evaluate(() => {
    const parts = [document.title, document.body.innerText];
    document.querySelectorAll('[aria-label],[placeholder],[title],img[alt],option').forEach((e) => {
      for (const a of ['aria-label', 'placeholder', 'title', 'alt']) {
        const v = e.getAttribute(a);
        if (v) parts.push(v);
      }
      if (e.tagName === 'OPTION') parts.push(e.textContent ?? '');
    });
    return parts.join('\n');
  });
}

/** The longest fixed piece of a text with `{placeholders}`. */
function longestPiece(text: string): string {
  return text.split(/\{\w+\}/).reduce((a, b) => (b.trim().length > a.trim().length ? b : a), '').trim();
}

/** The names of the two languages are shown in their own language in both: nothing else is allowed. */
const AUTONYMS = ['繁體中文'];

/**
 * No text of the other language is on the page: no CJK character in English (other than the
 * name of the language itself), and none of the English texts of the dictionary in Chinese
 * (unless the Chinese text contains that very word, like T.38, SIP or TIFF).
 */
function foreignTexts(text: string, loc: Loc): string[] {
  const found: string[] = [];
  if (loc === 'en') {
    let t = text;
    for (const a of AUTONYMS) t = t.split(a).join('');
    const cjk = t.match(/[\u3000-\u303f\u4e00-\u9fff\uff00-\uffef]+/g);
    if (cjk) found.push(...cjk.map((s) => `CJK: ${s}`));
    return found;
  }
  for (const [key, zh, en] of allTexts()) {
    const piece = longestPiece(en);
    if (piece.length < 5 || !/[A-Za-z]{4}/.test(piece)) continue;
    if (zh.includes(piece) || en === zh) continue;
    // as a word of its own (the label `User-Agent` does not contain the text `Agent`)
    const word = new RegExp(`(?<![\\w-])${piece.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\w-])`);
    if (word.test(text)) found.push(`${key}: "${piece}"`);
  }
  return found;
}

async function go(page: Page, hash: string) {
  await page.evaluate((h) => (location.hash = h), hash);
  await page.waitForLoadState('networkidle');
}

test('U17 switching zh-TW <-> en changes every visible text on every page', async ({ page }) => {
  const checked: string[] = [];
  const check = async (loc: Loc, what: string) => {
    await expect(page.getByText(tr('app.loading', {}, loc), { exact: true })).toHaveCount(0);
    const text = await readable(page);
    expect(text.length, `${what}: the page has text`).toBeGreaterThan(20);
    // the page really is in this language
    expect(foreignTexts(text, loc), `${what} (${loc}) shows text of the other language`).toEqual([]);
    checked.push(`${loc} ${what}`);
  };

  // the sign-in page, in both languages
  await page.goto('/');
  await expect(page.getByRole('button', { name: tr('login.submit') })).toBeVisible();
  await check('zh-TW', 'login');
  await page.locator('#login-locale').selectOption('en');
  await expect(page.getByRole('button', { name: tr('login.submit', {}, 'en') })).toBeVisible();
  await check('en', 'login');
  await page.locator('#login-locale').selectOption('zh-TW');

  // a fax to open
  await login(page, 'root');
  const fax = await ensureFax(page.request, '0912000111', 3);

  const pages: [string, string][] = [
    ['#/faxes', 'fax list'],
    [`#/faxes/${fax.id}`, 'fax'],
    ['#/status', 'status'],
    ['#/users', 'users'],
    ['#/audit', 'audit'],
    ['#/account', 'account'],
    ...['sip', 'media', 'call', 'capture', 'trunks', 'trunk:gw', 'routes', 'delivery', 'retention'].map((s): [string, string] => [`#/settings/${encodeURIComponent(s)}`, `settings ${s}`]),
  ];
  for (const loc of ['zh-TW', 'en'] as const) {
    // the switch in the menu bar
    await page.locator('#locale').selectOption(loc);
    await expect(page.getByRole('link', { name: tr('nav.faxes', {}, loc), exact: true })).toBeVisible();
    for (const [hash, what] of pages) {
      await go(page, hash);
      await check(loc, what);
    }
  }
  // both directions were covered on a page with data and one with a form
  expect(checked.length).toBe(2 + 2 * pages.length);

  // the control: the scan does find text of the other language (a Chinese page read as English)
  await page.locator('#locale').selectOption('zh-TW');
  await go(page, '#/faxes');
  expect(foreignTexts(await readable(page), 'en').length, 'the check can fail').toBeGreaterThan(0);
  await page.locator('#locale').selectOption('en');
  await go(page, '#/status');
  expect(foreignTexts(await readable(page), 'zh-TW').length, 'the check can fail').toBeGreaterThan(0);
});

test('U17 the language of the account is kept on the server and used in a fresh browser', async ({ browser }) => {
  const a = await signedIn(browser, 'amy');
  try {
    await a.page.getByRole('link', { name: 'amy', exact: true }).click();
    await a.page.locator('#acc-locale').selectOption('en');
    await a.page.getByRole('button', { name: tr('common.save'), exact: true }).click();
    await expect(a.page.getByText(tr('common.saved', {}, 'en'), { exact: true })).toBeVisible();
    expect(await sql('a', "select locale l from users where username = 'amy'")).toEqual([{ l: 'en' }]);

    // a new browser (Chinese browser language, nothing remembered): after the sign-in it is English
    const b = await signedIn(browser, 'amy');
    try {
      await expect(b.page.getByRole('link', { name: tr('nav.faxes', {}, 'en'), exact: true })).toBeVisible();
      await expect(b.page.getByRole('link', { name: tr('nav.faxes', {}, 'zh-TW'), exact: true })).toHaveCount(0);
    } finally {
      await b.context.close();
    }

    // back to "follow the browser": Chinese again, and null on the server
    await a.page.locator('#acc-locale').selectOption('');
    await a.page.getByRole('button', { name: tr('common.save', {}, 'en'), exact: true }).click();
    await expect(a.page.getByRole('link', { name: tr('nav.faxes', {}, 'zh-TW'), exact: true })).toBeVisible();
    expect(await sql('a', "select locale l from users where username = 'amy'")).toEqual([{ l: null }]);
  } finally {
    await a.context.close();
  }
});
