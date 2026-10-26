// M6a (docs/test-plans/M6a.md section 5): D02 (the Node.js example), D03 (the Java example) and
// the shell commands of docs/integration.md section 3 (HTTPS certificate), run for real against the
// HTTPS instance `b` of scripts/e2e-web-container.sh: a real sip-faxd, a fax sent by SpanDSP, a
// read-only key made with the CLI. The files the examples save are compared byte for byte with
// what the API returns.

import { execFileSync, spawnSync } from 'node:child_process';
import { X509Certificate } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { expect, request, test, type APIRequestContext } from '@playwright/test';
import { HTTPS_URL, PW, ensureFax, sh, signedIn } from './support';

test.describe.configure({ mode: 'serial' });

const BIN = '/work/target/docker/debug';
const CFG = '/tmp/e2e/b/sip-faxd.toml';
const ROOT = resolve('..');
const GUIDE = readFileSync(join(ROOT, 'docs', 'integration.md'), 'utf8').replace(/\r\n/g, '\n');
const BASE = `${HTTPS_URL}/api/v1`;
const ANI = '0919000062';
const KEY_RE = /^sfx_[A-Za-z0-9_-]{43}$/;

/** bash on every platform (Git Bash on Windows). */
function bash(cmd: string, cwd: string): string {
  return execFileSync('bash', ['-c', cmd], { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

/** The command of the guide that starts with \`name\` (written in backticks in a sentence or list). */
function guideCommand(name: string): string {
  const m = GUIDE.match(new RegExp('`(' + name + ' [^`]+)`'));
  if (!m) throw new Error(`the guide has no command ${name}`);
  return m[1]!;
}

let key = '';
let tmp = '';
let crt = '';
let apiKeyRequest: APIRequestContext;
let faxId = 0;

test.afterAll(async () => {
  await apiKeyRequest?.dispose();
});

test('the key (made with the CLI), a fax, and the certificate the way the guide says', async ({ browser }) => {
  tmp = mkdtempSync(join(tmpdir(), 'm6a-examples-'));
  // a fax on the HTTPS instance, made by SpanDSP
  const { context } = await signedIn(browser, 'root', { instance: 'b' });
  try {
    const fax = await ensureFax(context.request, ANI, 1, 'b');
    faxId = fax.id;
    expect(fax.fax_status).toBe('received');
    expect(fax.has_pdf).toBe(true);
  } finally {
    await context.close();
  }

  // the key: `sip-faxd apikey create` on the server, as the guide says
  const out = await sh([`${BIN}/sip-faxd`, 'apikey', 'create', 'examples', '--config', CFG]);
  key = out.split('\n').find((l) => l.startsWith('sfx_'))!.trim();
  expect(key).toMatch(KEY_RE);
  expect(out).toContain('fax:read');
  expect(out).not.toContain('fax:write');

  // the certificate: the command of the guide, with our host
  const cmd = guideCommand('openssl s_client').replace('fax.example.com:8443', new URL(HTTPS_URL).host);
  bash(cmd, tmp);
  crt = join(tmp, 'sip-faxd.crt');
  expect(existsSync(crt)).toBe(true);
  const pem = readFileSync(crt, 'utf8');
  expect(pem.startsWith('-----BEGIN CERTIFICATE-----')).toBe(true);
  // ... and it is the one the server logged at its start (the guide says to compare)
  const fp = new X509Certificate(pem).fingerprint256.replace(/:/g, '').toLowerCase();
  const log = await sh(['sh', '-c', 'grep "fingerprint" /tmp/e2e/b/serve.log']);
  expect(log.replace(/[:\s]/g, '').toLowerCase()).toContain(fp);
  // a request with the key and this certificate as the only trust: what an example does
  apiKeyRequest = await request.newContext({ ignoreHTTPSErrors: true, extraHTTPHeaders: { Authorization: `Bearer ${key}` } });
  const r = await apiKeyRequest.get(`${BASE}/faxes/${faxId}`);
  expect(r.status()).toBe(200);
  // the change feed holds back the last 2 seconds (docs/integration.md section 4): the examples
  // see the fax only after that
  await new Promise((r) => setTimeout(r, 2500));
});

/** What the example saved is what the API returns, byte for byte. */
async function expectSavedFiles(outDir: string, id: number) {
  const pdf = readFileSync(join(outDir, `${id}.pdf`));
  const png = readFileSync(join(outDir, `${id}-p1.png`));
  const apiPdf = Buffer.from(await (await apiKeyRequest.get(`${BASE}/faxes/${id}/file.pdf`)).body());
  const apiPng = Buffer.from(await (await apiKeyRequest.get(`${BASE}/faxes/${id}/pages/1.png?kind=thumb`)).body());
  expect(pdf.subarray(0, 5).toString('latin1')).toBe('%PDF-');
  expect(png.subarray(0, 8).toString('latin1')).toBe('\x89PNG\r\n\x1a\n');
  expect(pdf.equals(apiPdf), 'the saved PDF is the API\'s PDF').toBe(true);
  expect(png.equals(apiPng), 'the saved thumbnail is the API\'s thumbnail').toBe(true);
  const fax = (await (await apiKeyRequest.get(`${BASE}/faxes/${id}`)).json()) as { pdf_size: number };
  expect(pdf.length).toBe(fax.pdf_size);
  // no half-written file is left behind
  expect(readdirSync(outDir).filter((f) => f.endsWith('.part'))).toEqual([]);
}

interface Client {
  /** Runs the example once; `extra` replaces or adds environment variables. */
  run(env: Record<string, string>): { status: number | null; stdout: string; stderr: string };
}

/** The behaviour that both examples share (docs/test-plans/M6a.md D02, D03). */
async function exerciseExample(name: string, client: Client) {
  const outDir = join(tmp, `${name}-out`);
  const cursorFile = join(tmp, `${name}-cursor.txt`);
  const env = { FAX_BASE_URL: BASE, FAX_API_KEY: key, FAX_OUT_DIR: outDir, FAX_CURSOR_FILE: cursorFile };

  // 1. the first run: lists the faxes, saves the PDF and the thumbnail of every finished one
  const first = client.run(env);
  expect(first.status, `${first.stdout}\n${first.stderr}`).toBe(0);
  expect(first.stdout).toMatch(new RegExp(`fax ${faxId}: received, 1 pages, from ${ANI} `));
  expect(first.stdout).toContain(`saved ${join(outDir, `${faxId}.pdf`)}`);
  await expectSavedFiles(outDir, faxId);
  // every fax with a PDF got its files, none twice
  const listed = (await (await apiKeyRequest.get(`${BASE}/faxes?limit=200`)).json()) as { items: { id: number; has_pdf: boolean; fax_status: string }[] };
  for (const f of listed.items.filter((x) => x.fax_status !== 'receiving' && x.has_pdf)) {
    expect(existsSync(join(outDir, `${f.id}.pdf`)), `fax ${f.id} saved`).toBe(true);
  }

  // 2. the cursor was saved and is the end of the feed
  const cursor = readFileSync(cursorFile, 'utf8').trim();
  expect(cursor).toMatch(/^-?\d+\.\d+$/);
  const tail = (await (await apiKeyRequest.get(`${BASE}/faxes/changes?after=${cursor}`)).json()) as { items: unknown[]; next: string };
  expect(tail.items).toEqual([]);
  expect(tail.next).toBe(cursor);

  // 3. a second run: nothing new, nothing downloaded again
  const mtime = statSync(join(outDir, `${faxId}.pdf`)).mtimeMs;
  const second = client.run(env);
  expect(second.status, second.stderr).toBe(0);
  expect(second.stdout.trim()).toBe('');
  expect(statSync(join(outDir, `${faxId}.pdf`)).mtimeMs).toBe(mtime);

  // 4. the fax changes (the administrator marks it read): the example sees it once more, and
  //    does not fetch the files again (the changes of the last 2 seconds wait)
  const root = await request.newContext({ ignoreHTTPSErrors: true });
  try {
    const login = await root.post(`${HTTPS_URL}/api/v1/auth/login`, { data: { username: 'root', password: PW } });
    expect(login.status()).toBe(200);
    const csrfToken = ((await login.json()) as { csrf: string }).csrf;
    const cur = (await (await root.get(`${HTTPS_URL}/api/v1/faxes/${faxId}`)).json()) as { is_read: boolean };
    const put = await root.put(`${HTTPS_URL}/api/v1/faxes/${faxId}/read`, { headers: { 'X-Sipfax-Csrf': csrfToken }, data: { read: !cur.is_read } });
    expect(put.status()).toBe(200);
  } finally {
    await root.dispose();
  }
  await new Promise((r) => setTimeout(r, 2500));
  const third = client.run(env);
  expect(third.status, third.stderr).toBe(0);
  const lines = third.stdout.trim().split('\n');
  expect(lines.length, third.stdout).toBe(1);
  expect(lines[0]).toContain(`fax ${faxId}: received`);
  expect(third.stdout).not.toContain('saved');

  // 5. a start from nothing (no cursor file) sees the whole history again and finds its files there
  const again = client.run({ ...env, FAX_CURSOR_FILE: join(tmp, `${name}-cursor-2.txt`), FAX_OUT_DIR: join(tmp, `${name}-out-2`) });
  expect(again.status, again.stderr).toBe(0);
  await expectSavedFiles(join(tmp, `${name}-out-2`), faxId);
}

test('D02 the Node.js example, against the real sip-faxd over HTTPS with the certificate trusted as the guide says', async () => {
  const node: Client = {
    run: (env) => {
      const r = spawnSync(process.execPath, [join(ROOT, 'examples', 'node', 'fax-client.mjs'), '--once'], {
        env: { ...process.env, NODE_EXTRA_CA_CERTS: crt, ...env },
        encoding: 'utf8',
        timeout: 120_000,
      });
      return { status: r.status, stdout: r.stdout, stderr: r.stderr };
    },
  };
  await exerciseExample('node', node);

  // without the certificate the self-signed server is refused (the check is not switched off)
  const noCa = spawnSync(process.execPath, [join(ROOT, 'examples', 'node', 'fax-client.mjs'), '--once'], {
    env: { ...process.env, NODE_EXTRA_CA_CERTS: '', NODE_TLS_REJECT_UNAUTHORIZED: '1', FAX_BASE_URL: BASE, FAX_API_KEY: key, FAX_OUT_DIR: join(tmp, 'node-noca'), FAX_CURSOR_FILE: join(tmp, 'node-noca.txt') },
    encoding: 'utf8',
    timeout: 120_000,
  });
  expect(noCa.status).not.toBe(0);
  expect(existsSync(join(tmp, 'node-noca.txt'))).toBe(false);
  // missing settings are named
  const none = spawnSync(process.execPath, [join(ROOT, 'examples', 'node', 'fax-client.mjs'), '--once'], { env: { PATH: process.env.PATH ?? '' }, encoding: 'utf8' });
  expect(none.status).toBe(2);
  expect(none.stderr).toContain('FAX_BASE_URL');
});

test('D03 the Java example (and the JDBC one compiles), with the truststore built by the keytool command of the guide', async () => {
  // the truststore: the command of the guide, on our certificate
  const keytool = guideCommand('keytool');
  const store = join(tmp, 'fax-truststore.p12');
  // (the command as written: the files are in the working directory)
  const cmd = keytool;
  const kt = spawnSync('bash', ['-c', cmd], { cwd: tmp, encoding: 'utf8' });
  expect(kt.status, `${kt.stdout}${kt.stderr}`).toBe(0);
  expect(existsSync(store)).toBe(true);
  // the options of the run, as the guide writes them
  const props = [`-Djavax.net.ssl.trustStore=${store}`, '-Djavax.net.ssl.trustStorePassword=changeit'];
  const java: Client = {
    run: (env) => {
      const r = spawnSync('java', [...props, join(ROOT, 'examples', 'java', 'FaxClient.java'), '--once'], {
        env: { ...process.env, ...env },
        encoding: 'utf8',
        timeout: 180_000,
      });
      return { status: r.status, stdout: r.stdout, stderr: r.stderr };
    },
  };
  await exerciseExample('java', java);

  // without the truststore the self-signed server is refused
  const noStore = spawnSync('java', [join(ROOT, 'examples', 'java', 'FaxClient.java'), '--once'], {
    env: { ...process.env, FAX_BASE_URL: BASE, FAX_API_KEY: key, FAX_OUT_DIR: join(tmp, 'java-nostore'), FAX_CURSOR_FILE: join(tmp, 'java-nostore.txt') },
    encoding: 'utf8',
    timeout: 180_000,
  });
  expect(noStore.status).not.toBe(0);
  expect(existsSync(join(tmp, 'java-nostore.txt'))).toBe(false);

  // the JDBC example: it compiles (it needs a database and a driver to run, see D04 and V03 for the SQL)
  const javac = spawnSync('javac', ['-d', join(tmp, 'classes'), join(ROOT, 'examples', 'java', 'FaxDbReader.java')], { encoding: 'utf8' });
  expect(javac.status, `${javac.stdout}${javac.stderr}`).toBe(0);
});

test('a revoked key makes both examples stop with the message that says why', async () => {
  const out = await sh([`${BIN}/sip-faxd`, 'apikey', 'revoke', 'examples', '--config', CFG]);
  expect(out).toContain('revoked');
  const env = { ...process.env, NODE_EXTRA_CA_CERTS: crt, FAX_BASE_URL: BASE, FAX_API_KEY: key, FAX_OUT_DIR: join(tmp, 'revoked-out'), FAX_CURSOR_FILE: join(tmp, 'revoked.txt') };
  const node = spawnSync(process.execPath, [join(ROOT, 'examples', 'node', 'fax-client.mjs'), '--once'], { env, encoding: 'utf8', timeout: 120_000 });
  expect(node.status).toBe(2);
  expect(node.stderr).toContain('API key is not valid');
  const store = join(tmp, 'fax-truststore.p12');
  const java = spawnSync('java', [`-Djavax.net.ssl.trustStore=${store}`, '-Djavax.net.ssl.trustStorePassword=changeit', join(ROOT, 'examples', 'java', 'FaxClient.java'), '--once'], { env, encoding: 'utf8', timeout: 180_000 });
  expect(java.status).toBe(2);
  expect(`${java.stdout}${java.stderr}`).toMatch(/not valid|invalid_api_key/);
  // and nothing was written
  expect(existsSync(join(tmp, 'revoked.txt'))).toBe(false);
});
