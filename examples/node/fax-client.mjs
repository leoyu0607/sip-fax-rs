// sip-faxd API example (Node.js 18+, no packages): follow the change feed, download the PDF and
// the thumbnail of the first page of every fax that finished, remember where it got to.
//
//   FAX_BASE_URL=https://fax.example.com:8443/api/v1 FAX_API_KEY=sfx_... \
//   NODE_EXTRA_CA_CERTS=sip-faxd.crt node fax-client.mjs [--once]
//
// FAX_OUT_DIR (default ./faxes) receives <id>.pdf and <id>-p1.png; FAX_CURSOR_FILE (default
// ./fax-cursor.txt) keeps the position, so a restart continues where it stopped. With --once it
// stops when it has caught up, otherwise it asks again every 10 seconds.
// See docs/integration.md.

import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

const LIMIT = 100;

function required(name) {
  const v = process.env[name];
  if (!v) {
    console.error(`set ${name}`);
    process.exit(2);
  }
  return v;
}

const base = required('FAX_BASE_URL').replace(/\/+$/, '');
const key = required('FAX_API_KEY');
const outDir = process.env.FAX_OUT_DIR ?? 'faxes';
const cursorFile = process.env.FAX_CURSOR_FILE ?? 'fax-cursor.txt';
const once = process.argv.includes('--once');

/** GET with the key; API errors are {"error": {"code", "message"}}: decide by the code. */
async function get(path) {
  const r = await fetch(`${base}${path}`, {
    headers: { Authorization: `Bearer ${key}` },
    signal: AbortSignal.timeout(60_000),
  });
  if (r.ok) return r;
  const body = await r.text();
  let code = '?';
  try {
    code = JSON.parse(body).error.code;
  } catch {
    // not the API's JSON (a proxy in between, for example)
  }
  if (code === 'invalid_api_key') {
    console.error('the API key is not valid (revoked or expired?): ask the administrator');
    process.exit(2);
  }
  throw new Error(`HTTP ${r.status} ${code}: ${body}`);
}

/** Into a temporary file first: a broken download never leaves a half file behind. */
async function download(path, to) {
  const r = await get(path);
  await writeFile(`${to}.part`, Buffer.from(await r.arrayBuffer()));
  await rename(`${to}.part`, to);
}

/** One new or changed fax: print it; once it is finished, fetch its files (once). */
async function handle(fax) {
  console.log(
    `fax ${fax.id}: ${fax.fax_status}, ${fax.pages} pages, from ${fax.ani} to ${fax.dnis}, started ${fax.started_at}`,
  );
  if (fax.fax_status === 'receiving') return; // it comes again when the call ends
  const pdf = join(outDir, `${fax.id}.pdf`);
  if (fax.has_pdf && !existsSync(pdf)) {
    await download(`/faxes/${fax.id}/file.pdf`, pdf);
    if (fax.pages > 0) await download(`/faxes/${fax.id}/pages/1.png?kind=thumb`, join(outDir, `${fax.id}-p1.png`));
    console.log(`  saved ${pdf}`);
  }
}

await mkdir(outDir, { recursive: true });
let cursor = existsSync(cursorFile) ? (await readFile(cursorFile, 'utf8')).trim() : '';
for (;;) {
  const after = cursor === '' ? '' : `&after=${encodeURIComponent(cursor)}`;
  const page = await (await get(`/faxes/changes?limit=${LIMIT}${after}`)).json();
  for (const fax of page.items) await handle(fax);
  // saved after the faxes are handled: a crash repeats some, it never skips one
  cursor = page.next;
  await writeFile(cursorFile, cursor);
  if (page.items.length < LIMIT) {
    if (once) break;
    await sleep(10_000);
  }
}
