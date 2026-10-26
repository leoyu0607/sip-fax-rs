// The REST client of `/api/v1` (spec `docs/design/m4-web.md` 3). Every request that changes
// something is JSON and carries the CSRF token given at login (spec 2.3).

export const BASE = '/api/v1';

/** An answer of the API that is not 2xx, in its one error shape. */
export class ApiError extends Error {
  readonly status: number;
  /** The fixed code (`invalid_credentials`, `conflict`, ...); the UI translates this. */
  readonly code: string;
  readonly fields: Record<string, unknown> | undefined;
  /** Seconds from `Retry-After` (423 / 429). */
  readonly retryAfter: number | undefined;

  constructor(
    status: number,
    code: string,
    message: string,
    fields?: Record<string, unknown>,
    retryAfter?: number,
  ) {
    super(message);
    this.status = status;
    this.code = code;
    this.fields = fields;
    this.retryAfter = retryAfter;
  }
}

let csrf: string | null = null;

/** The CSRF token of the session (from login, password change or `/auth/me`). */
export function setCsrf(token: string | null): void {
  csrf = token;
}

/** Called when a request finds the session gone (401), so the app can show the login. */
let onUnauthenticated: (() => void) | null = null;
export function setOnUnauthenticated(f: (() => void) | null): void {
  onUnauthenticated = f;
}

function retryAfter(h: Headers): number | undefined {
  const v = h.get('Retry-After');
  if (v === null) return undefined;
  const n = Number.parseInt(v, 10);
  return Number.isFinite(n) ? n : undefined;
}

/** The error of a failed response; a body that is not the API's shape becomes `http_<status>`. */
export async function errorOf(r: Response): Promise<ApiError> {
  let code = `http_${r.status}`;
  let message = r.statusText;
  let fields: Record<string, unknown> | undefined;
  try {
    const j = (await r.json()) as { error?: { code?: string; message?: string; fields?: Record<string, unknown> } };
    if (j.error?.code) {
      code = j.error.code;
      message = j.error.message ?? message;
      fields = j.error.fields;
    }
  } catch {
    // not JSON
  }
  return new ApiError(r.status, code, message, fields, retryAfter(r.headers));
}

export async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  const changes = method !== 'GET' && method !== 'HEAD';
  if (changes) {
    headers['Content-Type'] = 'application/json';
    if (csrf !== null) headers['X-Sipfax-Csrf'] = csrf;
  }
  let r: Response;
  try {
    r = await fetch(BASE + path, {
      method,
      headers,
      credentials: 'same-origin',
      body: changes ? JSON.stringify(body ?? {}) : undefined,
    });
  } catch (e) {
    throw new ApiError(0, 'network', String(e));
  }
  if (!r.ok) {
    const e = await errorOf(r);
    if (e.status === 401 && !path.startsWith('/auth/login') && onUnauthenticated) onUnauthenticated();
    throw e;
  }
  if (r.status === 204) return undefined as T;
  return (await r.json()) as T;
}

export const get = <T>(path: string) => request<T>('GET', path);
export const post = <T>(path: string, body?: unknown) => request<T>('POST', path, body);
export const put = <T>(path: string, body?: unknown) => request<T>('PUT', path, body);
export const del = <T>(path: string) => request<T>('DELETE', path);

/** `?a=1&b=x` from the defined, non-empty values. */
export function query(params: Record<string, string | number | boolean | null | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === '') continue;
    q.set(k, String(v));
  }
  const s = q.toString();
  return s === '' ? '' : `?${s}`;
}

// ------------------------------------------------------------------------------ types

export type Role = 'admin' | 'agent';

export interface User {
  id: number;
  username: string;
  display_name: string | null;
  role: Role;
  locale: string | null;
  disabled: boolean;
  must_change_password: boolean;
  last_login_at: string | null;
  created_at: string;
}

/** An API key as listed (M6a): never the key itself. */
export interface ApiKey {
  id: number;
  name: string;
  prefix: string;
  scopes: string[];
  created_by: string;
  created_at: string;
  expires_at: string | null;
  revoked_at: string | null;
  revoked_by: string | null;
  last_used_at: string | null;
  last_used_ip: string | null;
  active: boolean;
}

export interface LoginOk {
  user: User;
  csrf: string;
  expires_at?: string;
}

export type FaxStatus = 'receiving' | 'received' | 'partial' | 'failed' | 'no_fax';

export interface Delivery {
  id: number;
  target: string;
  kind: string;
  attempt: number;
  status: string;
  last_error: string | null;
  next_try_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface Fax {
  id: number;
  call_id: string;
  trunk: string | null;
  ani: string | null;
  ani_source: string | null;
  dnis: string | null;
  dnis_source: string | null;
  remote_ident: string | null;
  started_at: string;
  answered_at: string | null;
  ended_at: string | null;
  duration_sec: number | null;
  hangup_direction: string | null;
  hangup_code: string | null;
  sip_status: number | null;
  t30_code: number | null;
  fax_status: FaxStatus;
  pages: number;
  resolution: string | null;
  bit_rate: number | null;
  compression: string | null;
  ecm: boolean | null;
  /** Modem of the last DCS: `v27ter`, `v29`, `v17` (0.5 and later). */
  modem?: string | null;
  /** `t38` or `g711` (0.5 and later). */
  media?: string | null;
  has_tiff: boolean;
  has_pdf: boolean;
  tiff_size: number | null;
  pdf_size: number | null;
  files_purged_at: string | null;
  is_read: boolean;
  read_changed_by: string | null;
  read_changed_at: string | null;
  note: string | null;
  note_updated_by: string | null;
  note_updated_at: string | null;
  created_at: string;
  updated_at: string;
  deliveries?: Delivery[];
}

export interface FaxPage {
  items: Fax[];
  next_cursor: string | null;
}

export interface Section {
  name: string;
  version: number;
  body: unknown;
}

export interface ConfigStatus {
  source: 'database' | 'file_fallback';
  pending_restart: string[];
  key?: unknown;
  undecryptable?: string[];
}

export interface ConfigList {
  sections: Section[];
  status: ConfigStatus;
  restart_fields?: string[];
}

export interface UpdateOutcome {
  section: string;
  version: number;
  restart_required: string[];
  pending_restart: string[];
  warnings: unknown[];
  applied: unknown;
}

export interface TrunkState {
  name: string;
  /** `none`, `registering`, `registered`, `failed: why`, ... */
  registration: string;
}

export interface ServiceStatus {
  version: string;
  /** `running`, or `stopping` while the calls are drained. */
  state: string;
  started_at: string;
  uptime_secs: number;
  listen: string[];
  active_calls: number;
  trunks: TrunkState[];
}

export interface Status {
  service: ServiceStatus;
  database: { state: 'ok' | 'unavailable'; schema?: number };
  config: ConfigStatus;
  /** The file system of the data directory; `null` when it cannot be read. */
  storage: { free_bytes: number; total_bytes: number } | null;
}

/** `GET /faxes/counts?since=…` */
export interface Counts {
  unread: number;
  since?: number;
  deliveries?: { failed: number; giving_up: number };
}

export interface AuditEntry {
  id: number;
  section: string;
  action: string;
  before: unknown;
  after: unknown;
  by: string;
  at: string;
}

export interface AuditPage {
  entries: AuditEntry[];
  next: number | null;
}
