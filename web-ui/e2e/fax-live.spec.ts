// U11 (a fax arrives while the list is open), U12 (opening a fax), U13 (two people edit the note).

import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { ensureFax, faxByAni, login, sql, signedIn, startFax, tr } from './support';

test.describe.configure({ mode: 'serial' });

const LIVE_ANI = '0912000111';

test('U11 a real 3-page T.38 fax is followed live: receiving page 1, 2, 3, then done, without a reload', async ({ page }) => {
  await login(page, 'amy');
  await page.goto('/#/faxes');
  await expect(page.getByRole('heading', { name: tr('faxes.title'), exact: true })).toBeVisible();
  await expect(page.getByText(tr('live.on'))).toBeAttached();

  // a watcher in the page records every state of this fax's status badge, however short-lived;
  // `marker` proves that the document is never reloaded
  await page.evaluate((ani) => {
    const w = window as unknown as { __marker: string; __seen: string[] };
    w.__marker = 'same document';
    w.__seen = [];
    const snap = () => {
      for (const tr of document.querySelectorAll('tr[data-fax-id]')) {
        if (!tr.textContent?.includes(ani)) continue;
        const badge = tr.querySelector('.badge')?.textContent?.replace(/\s+/g, ' ').trim();
        if (badge && w.__seen[w.__seen.length - 1] !== badge) w.__seen.push(badge);
      }
    };
    new MutationObserver(snap).observe(document.body, { subtree: true, childList: true, characterData: true });
    snap();
  }, LIVE_ANI);
  const navigations: string[] = [];
  page.on('framenavigated', (f) => navigations.push(f.url()));

  const sending = startFax({ from: LIVE_ANI, pages: 3, ident: 'U11LIVE' });
  const row = page.locator('tr[data-fax-id]').filter({ hasText: LIVE_ANI });
  await expect(row).toHaveCount(1);
  for (const n of [1, 2, 3]) {
    await expect(row.locator('.badge')).toContainText(tr('fax.receiving_page', { page: n }), { timeout: 60_000 });
  }
  await expect(row.locator('.badge')).toContainText(tr('fax.status.received'), { timeout: 60_000 });
  await sending.done;

  // what the page showed, in order
  const seen = await page.evaluate(() => (window as unknown as { __seen: string[] }).__seen);
  const shown = [1, 2, 3].map((n) => seen.findIndex((s) => s.includes(tr('fax.receiving_page', { page: n }))));
  expect(shown.every((i) => i >= 0), `all three states were shown: ${JSON.stringify(seen)}`).toBe(true);
  expect(shown[0]! < shown[1]! && shown[1]! < shown[2]!, JSON.stringify(seen)).toBe(true);
  expect(seen[seen.length - 1]).toContain(tr('fax.status.received'));
  // the finished row: 3 pages, the same document (no reload, no navigation), the live link still up
  await expect(row.locator('td.num')).toHaveText('3');
  expect(await page.evaluate(() => (window as unknown as { __marker: string }).__marker)).toBe('same document');
  expect(navigations, 'no navigation while the fax arrived').toEqual([]);
  await expect(page.getByText(tr('live.on'))).toBeAttached();
  // the database agrees
  const rows = await sql('a', 'select fax_status s, pages p, remote_ident i from faxes where ani = ?', [LIVE_ANI]);
  expect(rows).toEqual([{ s: 'received', p: 3, i: 'U11LIVE' }]);
});

test('U12 open a fax: thumbnails, the page, PDF and TIFF download, read toggle, note; opening does not mark it read', async ({ page }) => {
  await login(page, 'amy');
  const fax = await ensureFax(page.request, LIVE_ANI, 3);
  expect(fax.pages).toBe(3);
  expect(fax.is_read, 'the fax is new').toBe(false);

  await page.goto('/#/faxes');
  const row = page.locator(`tr[data-fax-id="${fax.id}"]`);
  await expect(row).toHaveClass(/unread/);
  await row.getByRole('link').click();
  await expect(page.getByRole('heading', { name: tr('fax.title', { ani: LIVE_ANI }), exact: true })).toBeVisible();

  // thumbnails and the page are real PNGs
  const thumbs = page.locator('.thumbs img');
  await expect(thumbs).toHaveCount(3);
  await expect
    .poll(() => thumbs.evaluateAll((imgs) => imgs.map((i) => (i as HTMLImageElement).naturalWidth)))
    .toEqual([expect.any(Number), expect.any(Number), expect.any(Number)]);
  const widths = await thumbs.evaluateAll((imgs) => imgs.map((i) => (i as HTMLImageElement).naturalWidth));
  expect(widths.every((w) => w > 0), `thumbnail widths ${widths}`).toBe(true);
  const big = page.locator('.scroller img');
  await expect.poll(() => big.evaluate((i) => (i as HTMLImageElement).naturalWidth)).toBeGreaterThan(1000);
  const src = await big.getAttribute('src');
  const png = await page.request.get(src!);
  expect(png.headers()['content-type']).toBe('image/png');
  expect([...(await png.body()).subarray(0, 4)]).toEqual([0x89, 0x50, 0x4e, 0x47]);

  // pages: button, arrow key, thumbnail
  await expect(page.getByText(tr('fax.page_of', { page: 1, pages: 3 }))).toBeVisible();
  await page.getByRole('button', { name: tr('fax.next_page'), exact: true }).click();
  await expect(page.getByText(tr('fax.page_of', { page: 2, pages: 3 }))).toBeVisible();
  await expect(big).toHaveAttribute('src', /pages\/2\.png/);
  await page.keyboard.press('ArrowRight');
  await expect(page.getByText(tr('fax.page_of', { page: 3, pages: 3 }))).toBeVisible();
  await expect(page.getByRole('button', { name: tr('fax.next_page'), exact: true })).toBeDisabled();
  await thumbs.first().click();
  await expect(page.getByText(tr('fax.page_of', { page: 1, pages: 3 }))).toBeVisible();

  // opening it did not mark it read, here, on the server, or in the list
  expect((await faxByAni(page.request, LIVE_ANI)).is_read).toBe(false);
  expect(await sql('a', 'select is_read r from faxes where id = ?', [fax.id])).toEqual([{ r: 0 }]);
  await expect(page.locator('.page-head .badge.unread')).toContainText(tr('fax.unread'));

  // downloads: the bytes are the files the server stored
  const [pdf] = await Promise.all([page.waitForEvent('download'), page.getByRole('link', { name: tr('fax.download_pdf') }).click()]);
  expect(pdf.suggestedFilename()).toMatch(/\.pdf$/);
  const pdfBytes = readFileSync((await pdf.path())!);
  expect(pdfBytes.subarray(0, 5).toString('latin1')).toBe('%PDF-');
  expect(pdfBytes.length).toBe(fax.pdf_size);
  const [tif] = await Promise.all([page.waitForEvent('download'), page.getByRole('link', { name: tr('fax.download_tiff') }).click()]);
  expect(tif.suggestedFilename()).toMatch(/\.tif$/);
  const tifBytes = readFileSync((await tif.path())!);
  expect(['II*\0', 'MM\0*']).toContain(tifBytes.subarray(0, 4).toString('latin1'));
  expect(tifBytes.length).toBe(fax.tiff_size);

  // read / unread
  await page.getByRole('button', { name: tr('fax.mark_read'), exact: true }).click();
  await expect(page.locator('.page-head .badge:not(.unread)').last()).toContainText(tr('fax.read'));
  await expect.poll(async () => (await faxByAni(page.request, LIVE_ANI)).is_read).toBe(true);
  const read = await faxByAni(page.request, LIVE_ANI);
  expect(read.read_changed_by).toBe('amy');
  await page.getByRole('button', { name: tr('fax.mark_unread'), exact: true }).click();
  await expect.poll(async () => (await faxByAni(page.request, LIVE_ANI)).is_read).toBe(false);

  // the note is saved and survives a reload
  const text = 'call back before noon';
  await page.locator('#note').fill(text);
  await page.getByRole('button', { name: tr('fax.note_save'), exact: true }).click();
  await expect(page.getByText(tr('common.saved'), { exact: true })).toBeVisible();
  expect((await faxByAni(page.request, LIVE_ANI)).note).toBe(text);
  await page.reload();
  await expect(page.locator('#note')).toHaveValue(text);
  // ... and shows in the list
  await page.goto('/#/faxes');
  await expect(page.locator(`tr[data-fax-id="${fax.id}"] .note-cell`)).toHaveText(text);
});

test('U13 two browsers edit the same note: the second saver gets the conflict with the other version', async ({ browser }) => {
  const ANI = '0913000013';
  const amy = await signedIn(browser, 'amy');
  const ben = await signedIn(browser, 'ben');
  try {
    const fax = await ensureFax(amy.page.request, ANI, 1);
    const url = `/#/faxes/${fax.id}`;
    for (const p of [amy.page, ben.page]) {
      await p.goto(url);
      await expect(p.locator('#note')).toBeVisible();
    }
    const note = async () => (await sql('a', 'select note n, note_updated_by b from faxes where id = ?', [fax.id]))[0];
    const save = (p: typeof amy.page) => p.getByRole('button', { name: tr('fax.note_save'), exact: true });

    // ben starts typing, amy saves first
    await ben.page.locator('#note').fill('ben draft');
    await amy.page.locator('#note').fill('amy first');
    await save(amy.page).click();
    await expect(amy.page.getByText(tr('common.saved'), { exact: true })).toBeVisible();
    expect(await note()).toEqual({ n: 'amy first', b: 'amy' });
    // ben's text is not replaced under his hands
    await expect(ben.page.locator('#note')).toHaveValue('ben draft');

    // ben saves: refused, his text is not stored, amy's version is shown
    await save(ben.page).click();
    const conflict = ben.page.locator('.conflict');
    await expect(conflict).toContainText(tr('fax.note_conflict', { by: 'amy' }));
    await expect(conflict.locator('blockquote')).toHaveText('amy first');
    expect(await note()).toEqual({ n: 'amy first', b: 'amy' });
    // take theirs
    await conflict.getByRole('button', { name: tr('fax.note_take_theirs'), exact: true }).click();
    await expect(conflict).toHaveCount(0);
    await expect(ben.page.locator('#note')).toHaveValue('amy first');

    // again: ben edits from amy's version, amy changes it meanwhile, ben overwrites on purpose
    await ben.page.locator('#note').fill('ben second');
    await amy.page.locator('#note').fill('amy third');
    await save(amy.page).click();
    await expect(amy.page.getByText(tr('common.saved'), { exact: true })).toBeVisible();
    await save(ben.page).click();
    await expect(conflict).toContainText(tr('fax.note_conflict', { by: 'amy' }));
    await expect(conflict.locator('blockquote')).toHaveText('amy third');
    expect(await note()).toEqual({ n: 'amy third', b: 'amy' });
    await conflict.getByRole('button', { name: tr('fax.note_keep_mine'), exact: true }).click();
    await expect(conflict).toHaveCount(0);
    await expect.poll(note).toEqual({ n: 'ben second', b: 'ben' });
    // amy was not typing: her box follows without a reload
    await expect(amy.page.locator('#note')).toHaveValue('ben second');
  } finally {
    await amy.context.close();
    await ben.context.close();
  }
});
