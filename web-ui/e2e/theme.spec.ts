// U21: the theme switch in the header (system / light / dark), kept in this browser.

import { expect, test, type Page } from '@playwright/test';
import { login, signOut, signedIn, tr, userMenu } from './support';

const LIGHT_BG = 'rgb(246, 247, 249)'; // --bg of app.css
const DARK_BG = 'rgb(18, 22, 28)';

const bg = (page: Page) => page.evaluate(() => getComputedStyle(document.body).backgroundColor);

test('U21 the theme follows the system until light or dark is chosen, and the choice survives a reload and sign-in', async ({ browser }) => {
  const { context, page } = await signedIn(browser, 'root');
  try {
    await userMenu(page);
    const theme = page.locator('#theme');
    await expect(theme).toHaveValue('system');
    await expect(theme.locator('option')).toHaveText([tr('theme.system'), tr('theme.light'), tr('theme.dark')]);

    // system: the colours follow the system's scheme
    await page.emulateMedia({ colorScheme: 'light' });
    expect(await bg(page)).toBe(LIGHT_BG);
    await page.emulateMedia({ colorScheme: 'dark' });
    expect(await bg(page)).toBe(DARK_BG);

    // light on a dark system, dark on a light system
    await theme.selectOption('light');
    expect(await bg(page)).toBe(LIGHT_BG);
    await page.emulateMedia({ colorScheme: 'light' });
    await theme.selectOption('dark');
    expect(await bg(page)).toBe(DARK_BG);

    // kept over a reload, and already on the sign-in page (before the app has a session)
    await page.reload();
    await userMenu(page);
    await expect(page.locator('#theme')).toHaveValue('dark');
    expect(await bg(page)).toBe(DARK_BG);
    await signOut(page);
    await expect(page.getByRole('button', { name: tr('login.submit') })).toBeVisible();
    expect(await bg(page)).toBe(DARK_BG);

    // back to the system through the switch: nothing stored any more
    await login(page, 'root');
    await userMenu(page);
    await page.locator('#theme').selectOption('system');
    expect(await bg(page)).toBe(LIGHT_BG);
    expect(await page.evaluate(() => [localStorage.getItem('sipfax.theme'), document.documentElement.hasAttribute('data-theme')])).toEqual([null, false]);
    await page.reload();
    await userMenu(page);
    await expect(page.locator('#theme')).toHaveValue('system');
    expect(await bg(page)).toBe(LIGHT_BG);
  } finally {
    await context.close();
  }
});
