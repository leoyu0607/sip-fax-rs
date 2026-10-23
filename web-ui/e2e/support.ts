// Shared helpers of the E2E cases: the Docker container of scripts/e2e-web.sh (faxes are sent
// and the database is read from inside it), logins, and texts from the locale files.

import { spawn, type ChildProcess } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { expect, type APIRequestContext, type Browser, type BrowserContext, type Page } from '@playwright/test';
import type { Fax } from '../src/lib/api';

export const CONTAINER = process.env.E2E_CONTAINER ?? 'sipfax-e2e-web';
export const HTTP_URL = process.env.E2E_HTTP_URL ?? 'http://127.0.0.1:18080';
export const HTTPS_URL = process.env.E2E_HTTPS_URL ?? 'https://127.0.0.1:18443';
/** The instance without any trunk (a new installation; M4f F05). */
export const NOTRUNK_URL = process.env.E2E_NOTRUNK_URL ?? 'http://127.0.0.1:18081';
/** The password of every account made by scripts/e2e-web-container.sh. */
export const PW = 'Correct-horse-battery-1';

const BIN = '/work/target/docker/debug';

// ------------------------------------------------------------------------------ texts

type Dict = { [k: string]: string | Dict };

// (read as files: Node wants an import attribute for JSON modules)
const en = JSON.parse(readFileSync('src/locales/en.json', 'utf8')) as Dict;
const zh = JSON.parse(readFileSync('src/locales/zh-TW.json', 'utf8')) as Dict;

function lookup(d: Dict, key: string): string {
  let cur: string | Dict = d;
  for (const part of key.split('.')) {
    if (typeof cur === 'string' || cur[part] === undefined) throw new Error(`no text for ${key}`);
    cur = cur[part];
  }
  if (typeof cur !== 'string') throw new Error(`${key} is not a text`);
  return cur;
}

/** The text of `key` in `locale`, with `{name}` placeholders filled. */
export function tr(key: string, params: Record<string, string | number> = {}, locale: 'zh-TW' | 'en' = 'zh-TW'): string {
  const text = lookup(locale === 'en' ? en : zh, key);
  return text.replace(/\{(\w+)\}/g, (_, n: string) => String(params[n] ?? `{${n}}`));
}

/** Every text of both dictionaries as `[key, zh-TW, en]`. */
export function allTexts(): [string, string, string][] {
  const out: [string, string, string][] = [];
  const walk = (a: Dict, b: Dict, prefix: string) => {
    for (const [k, v] of Object.entries(a)) {
      const w = b[k];
      if (typeof v === 'string' && typeof w === 'string') out.push([`${prefix}${k}`, v, w]);
      else if (typeof v === 'object' && typeof w === 'object') walk(v, w, `${prefix}${k}.`);
    }
  };
  walk(zh, en, '');
  return out;
}

// ------------------------------------------------------------------------------ container

/** Runs a command in the test container; `input` goes to its stdin. */
export function sh(args: string[], input?: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn('docker', ['exec', ...(input !== undefined ? ['-i'] : []), CONTAINER, ...args]);
    let out = '';
    let err = '';
    child.stdout.on('data', (d: Buffer) => (out += d.toString()));
    child.stderr.on('data', (d: Buffer) => (err += d.toString()));
    child.on('error', reject);
    child.on('exit', (code) => (code === 0 ? resolve(out) : reject(new Error(`${args.join(' ')} exited with ${code}: ${err}`))));
    if (input !== undefined) child.stdin.end(input);
  });
}

export type Instance = 'a' | 'b' | 'c';
const SIP: Record<Instance, string> = { a: '127.0.0.1:5060', b: '127.0.0.1:5061', c: '127.0.0.1:5062' };

export interface FaxOptions {
  /** The calling number (From user), as the server extracts it. */
  from?: string;
  /** The called number (To and Request-URI user). */
  to?: string;
  /** The station id the sender transmits (the TSI). */
  ident?: string;
  /** 1 (short) or 3 pages (tall: each page takes a few seconds). */
  pages?: 1 | 3;
  instance?: Instance;
}

function uacArgs(o: FaxOptions): string[] {
  const tif = o.pages === 3 ? '/tmp/e2e/three.tif' : '/tmp/e2e/one.tif';
  const args = ['--target', SIP[o.instance ?? 'a'], '--mode', 'gateway', '--fax', tif];
  if (o.from) args.push('--from', o.from);
  if (o.to) args.push('--to', o.to);
  if (o.ident) args.push('--ident', o.ident);
  return args;
}

/** A fax being sent by SpanDSP (`sip-uac`) from inside the container. */
export interface Sending {
  /** Resolves with the sender's output when SpanDSP reported success; rejects otherwise. */
  done: Promise<string>;
}

export function startFax(o: FaxOptions = {}): Sending {
  const child: ChildProcess = spawn('docker', ['exec', CONTAINER, `${BIN}/sip_uac`, ...uacArgs(o)]);
  let out = '';
  child.stdout?.on('data', (d: Buffer) => (out += d.toString()));
  child.stderr?.on('data', (d: Buffer) => (out += d.toString()));
  const done = new Promise<string>((resolve, reject) => {
    child.on('exit', (code) => (code === 0 ? resolve(out) : reject(new Error(`sip-uac exited with ${code}:\n${out}`))));
    child.on('error', reject);
  });
  // a rejection nobody awaits yet must not stop the run
  done.catch(() => undefined);
  return { done };
}

/** Sends a fax and waits until it was received. */
export async function sendFax(o: FaxOptions = {}): Promise<void> {
  await startFax(o).done;
}

/** Rows of an SQL query on the database of an instance (read-only). */
export async function sql(instance: Instance, query: string, params: unknown[] = []): Promise<Record<string, unknown>[]> {
  const py = [
    'import json,sqlite3,sys',
    `c=sqlite3.connect("file:/tmp/e2e/${instance}/fax.db?mode=ro",uri=True,timeout=10)`,
    'c.row_factory=sqlite3.Row',
    'a=json.loads(sys.stdin.read())',
    'print(json.dumps([dict(r) for r in c.execute(a["q"],a["p"]).fetchall()]))',
  ].join('\n');
  const out = await sh(['python3', '-c', py], JSON.stringify({ q: query, p: params }));
  return JSON.parse(out) as Record<string, unknown>[];
}

// ------------------------------------------------------------------------------ pages

export function base(instance: Instance): string {
  return instance === 'a' ? HTTP_URL : instance === 'b' ? HTTPS_URL : NOTRUNK_URL;
}

/** Signs in through the form. */
export async function login(page: Page, user: string, password = PW): Promise<void> {
  await page.goto('/');
  await page.getByLabel(tr('login.username')).fill(user);
  await page.getByLabel(tr('login.password')).fill(password);
  await page.getByRole('button', { name: tr('login.submit') }).click();
  // the menu bar (in whichever language the account asks for)
  await expect(page.locator('header.topbar')).toBeVisible();
}

/** A signed-in browser context (own cookies) of `user`. */
export async function signedIn(browser: Browser, user: string, opts: { instance?: Instance; password?: string } = {}): Promise<{ context: BrowserContext; page: Page }> {
  const instance = opts.instance ?? 'a';
  const context = await browser.newContext({
    baseURL: base(instance),
    locale: 'zh-TW',
    timezoneId: 'Asia/Taipei',
    ignoreHTTPSErrors: instance === 'b',
  });
  const page = await context.newPage();
  await login(page, user, opts.password);
  return { context, page };
}

/** The CSRF token of the session of `request` (cookies shared with its page). */
export async function csrf(request: APIRequestContext): Promise<string> {
  const r = await request.get('/api/v1/auth/me');
  expect(r.status()).toBe(200);
  return ((await r.json()) as { csrf: string }).csrf;
}

/** The newest fax with the calling number `ani` (through the API of a signed-in `request`). */
export async function faxByAni(request: APIRequestContext, ani: string): Promise<Fax> {
  const r = await request.get(`/api/v1/faxes?ani=${encodeURIComponent(ani)}&limit=1`);
  expect(r.status()).toBe(200);
  const items = ((await r.json()) as { items: Fax[] }).items;
  expect(items.length, `a fax from ${ani}`).toBeGreaterThan(0);
  return items[0]!;
}

/** Sends a fax from `ani` unless the list already has one. */
export async function ensureFax(request: APIRequestContext, ani: string, pages: 1 | 3 = 1, instance: Instance = 'a'): Promise<Fax> {
  const r = await request.get(`/api/v1/faxes?ani=${encodeURIComponent(ani)}&limit=1`);
  const items = ((await r.json()) as { items: Fax[] }).items;
  if (items.length === 0) await sendFax({ from: ani, pages, instance });
  return faxByAni(request, ani);
}

/** Opens the account menu of the header (account page, language, theme, sign out) if it is closed. */
export async function userMenu(page: Page): Promise<void> {
  const menu = page.locator('details.user-menu');
  if (!(await menu.evaluate((d) => (d as HTMLDetailsElement).open))) await menu.locator('summary').click();
  await expect(menu.locator('.menu-panel')).toBeVisible();
}

/** Signs out through the account menu. */
export async function signOut(page: Page): Promise<void> {
  await userMenu(page);
  await page.getByRole('button', { name: tr('nav.logout'), exact: true }).click();
}
