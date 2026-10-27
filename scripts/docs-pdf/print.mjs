// Prints HTML files to PDF with the Chromium of Playwright (installed by web-ui's npm ci). Run it
// from web-ui/, where Playwright is installed:
//   node ../scripts/docs-pdf/print.mjs <in.html> <out.pdf> <footer text> [...more triples]
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const { chromium } = createRequire(resolve('package.json'))('playwright');

const args = process.argv.slice(2);
if (args.length === 0 || args.length % 3 !== 0) {
  console.error('usage: print.mjs <in.html> <out.pdf> <footer> [...]');
  process.exit(2);
}
const browser = await chromium.launch();
try {
  for (let i = 0; i < args.length; i += 3) {
    const [input, output, footer] = args.slice(i, i + 3);
    const page = await browser.newPage();
    await page.goto(pathToFileURL(resolve(input)).href, { waitUntil: 'load' });
    await page.pdf({
      path: output,
      format: 'A4',
      printBackground: true,
      preferCSSPageSize: true,
      displayHeaderFooter: true,
      headerTemplate: '<span></span>',
      footerTemplate: `<div style="width:100%;font-size:8pt;color:#57606a;padding:0 16mm;display:flex;justify-content:space-between;font-family:'Microsoft JhengHei','Noto Sans CJK TC',sans-serif"><span>${footer}</span><span><span class="pageNumber"></span> / <span class="totalPages"></span></span></div>`,
    });
    await page.close();
    console.log(`  ${output}`);
  }
} finally {
  await browser.close();
}
