// What the settings forms show of each section (spec 6, page 4). The server is the one
// validator (spec 0): these definitions only choose the input of each field, so that a value
// that is `null` today (no type in the JSON) still gets the right one. A key the server sends
// that is not listed here is kept as it is and sent back unchanged.

export type Field =
  | { key: string; kind: 'text'; nullable?: boolean; placeholder?: string; multiline?: boolean }
  | { key: string; kind: 'number'; nullable?: boolean; step?: number; min?: number; max?: number }
  | { key: string; kind: 'bool' }
  | { key: string; kind: 'select'; options: readonly string[]; nullable?: boolean }
  /** An array of strings, one per line; `options`: the values allowed, in order (checkboxes). */
  | { key: string; kind: 'list'; options?: readonly string[]; nullable?: boolean; ordered?: boolean }
  /** `{"set": bool}` when read; a new string, the same object (keep) or null (clear) when written. */
  | { key: string; kind: 'secret' }
  /** `{name: value}`, one `name: value` per line. */
  | { key: string; kind: 'map' }
  | { key: string; kind: 'group'; fields: readonly Field[] };

export type SectionKind = 'sip' | 'media' | 'call' | 'capture' | 'delivery' | 'retention' | 'trunk';

export const SIMPLE_SECTIONS = ['sip', 'media', 'call', 'capture', 'delivery', 'retention'] as const;

export const SCHEMAS: Record<SectionKind, readonly Field[]> = {
  sip: [
    { key: 'bind', kind: 'text', placeholder: '0.0.0.0:5060' },
    { key: 'transports', kind: 'list', options: ['udp', 'tcp'], nullable: true },
    { key: 'advertised', kind: 'text', nullable: true },
    { key: 'user_agent', kind: 'text', nullable: true },
    { key: 'allow', kind: 'text', nullable: true },
    { key: 'supported', kind: 'text', nullable: true },
    { key: 'unknown_source', kind: 'select', options: ['drop', 'reject', 'log'] },
  ],
  media: [
    { key: 'address', kind: 'text', nullable: true },
    { key: 'audio_port', kind: 'number', min: 1, max: 65535 },
    { key: 'port_range', kind: 'text', placeholder: '30000-30999' },
  ],
  call: [
    { key: 'send_ringing', kind: 'bool' },
    { key: 'answer_delay_ms', kind: 'number', min: 0 },
    { key: 'max_duration_secs', kind: 'number', min: 1 },
    { key: 'fax_session', kind: 'select', options: ['t38', 'hold'] },
    { key: 'station_id', kind: 'text' },
    { key: 'reinvite_timeout_secs', kind: 'number', min: 1 },
    { key: 'max_491_retries', kind: 'number', min: 0 },
  ],
  capture: [
    { key: 'enabled', kind: 'bool' },
    { key: 'dir', kind: 'text' },
    { key: 'keep', kind: 'select', options: ['all', 'failed'] },
    { key: 'udptl', kind: 'select', options: ['summary', 'off', 'full'] },
    { key: 'max_bytes_per_call', kind: 'number', min: 0 },
  ],
  delivery: [
    { key: 'catch_up_hours', kind: 'number', min: 0 },
    { key: 'max_attempts', kind: 'number', min: 1 },
  ],
  retention: [
    { key: 'files_days', kind: 'number', min: 0 },
    { key: 'records_days', kind: 'number', min: 0 },
    { key: 'run_at', kind: 'text', placeholder: 'HH:MM' },
  ],
  trunk: [
    { key: 'name', kind: 'text' },
    { key: 'preset', kind: 'select', options: ['generic', 'cht-cs210'] },
    { key: 'peer', kind: 'text', placeholder: '203.0.113.10:5060' },
    { key: 'allowed_sources', kind: 'list' },
    { key: 'transport', kind: 'select', options: ['udp', 'tcp'] },
    { key: 't38_policy', kind: 'select', options: ['auto', 'wait-peer', 'initiate', 'audio-only'] },
    { key: 'initiate_after_secs', kind: 'number', min: 0, step: 0.1 },
    { key: 'audio_codecs', kind: 'list', options: ['PCMU', 'PCMA'], ordered: true },
    {
      key: 'numbers',
      kind: 'group',
      fields: [
        { key: 'ani_sources', kind: 'list', options: ['pai', 'rpid', 'from'], ordered: true },
        { key: 'dnis_sources', kind: 'list', options: ['ruri', 'to', 'diversion'], ordered: true },
        { key: 'strip_plus', kind: 'bool' },
        { key: 'ani_strip_prefix', kind: 'text', nullable: true },
        { key: 'dnis_strip_prefix', kind: 'text', nullable: true },
      ],
    },
    {
      key: 'fax',
      kind: 'group',
      fields: [
        { key: 'resolutions', kind: 'list', options: ['standard', 'fine'] },
        { key: 'ecm', kind: 'bool' },
      ],
    },
    {
      key: 't38',
      kind: 'group',
      fields: [
        { key: 'max_version', kind: 'number', min: 0, max: 3 },
        { key: 'max_bit_rate', kind: 'number', min: 2400 },
        { key: 'rate_management', kind: 'select', options: ['transferredTCF', 'localTCF'] },
        { key: 'max_buffer', kind: 'number', min: 1 },
        { key: 'max_datagram', kind: 'number', min: 1 },
        { key: 'udp_ec', kind: 'select', options: ['redundancy', 'fec', 'none'] },
        { key: 'fec', kind: 'bool' },
        { key: 'redundancy_depth', kind: 'number', min: 0 },
        { key: 'fec_span', kind: 'number', min: 1 },
        { key: 'fec_entries', kind: 'number', min: 1 },
        { key: 'fill_bit_removal', kind: 'bool' },
        { key: 'transcoding_mmr', kind: 'bool' },
        { key: 'transcoding_jbig', kind: 'bool' },
      ],
    },
    {
      key: 'compat',
      kind: 'group',
      fields: [
        { key: 'accept_unknown_require', kind: 'bool' },
        { key: 'min_max_datagram', kind: 'number', min: 1 },
      ],
    },
    {
      key: 'register',
      kind: 'group',
      fields: [
        { key: 'enabled', kind: 'bool' },
        { key: 'registrar', kind: 'text', nullable: true },
        { key: 'domain', kind: 'text', nullable: true },
        { key: 'aor', kind: 'text', nullable: true },
        { key: 'username', kind: 'text', nullable: true },
        { key: 'password', kind: 'secret' },
        { key: 'expires', kind: 'number', min: 1 },
        { key: 'backoff_initial_secs', kind: 'number', min: 0, step: 0.1 },
        { key: 'backoff_max_secs', kind: 'number', min: 0, step: 0.1 },
      ],
    },
  ],
};

/** The match conditions of a route (`routes` section). */
export const ROUTE_FIELDS: readonly Field[] = [
  { key: 'name', kind: 'text', nullable: true },
  { key: 'dnis', kind: 'text', nullable: true },
  { key: 'dnis_prefix', kind: 'text', nullable: true },
  { key: 'trunk', kind: 'text', nullable: true },
  { key: 'default', kind: 'bool' },
  { key: 'deliver_partial', kind: 'bool' },
];

export const DELIVERY_KINDS = ['folder', 'email', 'webhook'] as const;
export type DeliveryKind = (typeof DELIVERY_KINDS)[number];

export const DELIVERY_FIELDS: Record<DeliveryKind, readonly Field[]> = {
  folder: [
    { key: 'dir', kind: 'text' },
    { key: 'filename', kind: 'text' },
    { key: 'formats', kind: 'list', options: ['pdf', 'tiff'] },
    { key: 'create_dir', kind: 'bool' },
  ],
  email: [
    { key: 'host', kind: 'text' },
    { key: 'port', kind: 'number', nullable: true, min: 1, max: 65535 },
    { key: 'security', kind: 'select', options: ['starttls', 'smtps', 'none'] },
    { key: 'tls_insecure', kind: 'bool' },
    { key: 'username', kind: 'text', nullable: true },
    { key: 'password', kind: 'secret' },
    { key: 'from', kind: 'text' },
    { key: 'to', kind: 'list' },
    { key: 'subject', kind: 'text' },
    { key: 'body', kind: 'text', multiline: true },
    { key: 'attachment_name', kind: 'text', nullable: true },
    { key: 'attach_tiff', kind: 'bool' },
    { key: 'timeout_secs', kind: 'number', min: 1 },
  ],
  webhook: [
    { key: 'url', kind: 'text', placeholder: 'https://' },
    { key: 'secret', kind: 'secret' },
    { key: 'signature_header', kind: 'text' },
    { key: 'attach_pdf', kind: 'bool' },
    { key: 'headers', kind: 'map' },
    { key: 'timeout_secs', kind: 'number', min: 1 },
  ],
};

/** What a new delivery of `kind` starts with (the server's defaults). */
export function newDelivery(kind: DeliveryKind): Record<string, unknown> {
  switch (kind) {
    case 'folder':
      return { kind, dir: '', filename: '{date}_{time}_{ani}_{id}', formats: ['pdf', 'tiff'], create_dir: true };
    case 'email':
      return {
        kind,
        host: '',
        port: null,
        security: 'starttls',
        tls_insecure: false,
        username: null,
        password: null,
        from: '',
        to: [],
        subject: 'Fax from {ani} ({pages} pages)',
        body: 'Fax received.\n\nFrom: {ani}\nTo: {dnis}\nTrunk: {trunk}\nTime: {started_at}\nPages: {pages}\nStatus: {status}\nID: {id}\n',
        attachment_name: null,
        attach_tiff: false,
        timeout_secs: 30,
      };
    case 'webhook':
      return { kind, url: '', secret: null, signature_header: 'X-Sipfax-Signature', attach_pdf: false, headers: {}, timeout_secs: 30 };
  }
}

export function newRoute(): Record<string, unknown> {
  return { name: null, dnis: null, dnis_prefix: null, trunk: null, default: false, deliver_partial: false, deliveries: [] };
}

/** A new trunk: the server fills in what the preset gives (only the name and peer are needed). */
export function newTrunk(name: string): Record<string, unknown> {
  return { name, preset: 'generic', peer: '' };
}

// ------------------------------------------------------------------------------ values

/** The value at the dotted `path` (`t38.max_version`, `routes[1].deliveries[0].url`). */
export function getPath(obj: unknown, path: string): unknown {
  let cur: unknown = obj;
  for (const part of splitPath(path)) {
    if (cur === null || typeof cur !== 'object') return undefined;
    cur = (cur as Record<string | number, unknown>)[part];
  }
  return cur;
}

/** `obj` with `value` at `path` (copies along the path; `obj` is not changed). */
export function setPath<T>(obj: T, path: string, value: unknown): T {
  const parts = splitPath(path);
  const go = (cur: unknown, i: number): unknown => {
    if (i === parts.length) return value;
    const k = parts[i] as string | number;
    const base: Record<string | number, unknown> | unknown[] = Array.isArray(cur)
      ? [...cur]
      : cur !== null && typeof cur === 'object'
        ? { ...(cur as Record<string, unknown>) }
        : typeof k === 'number'
          ? []
          : {};
    (base as Record<string | number, unknown>)[k] = go((base as Record<string | number, unknown>)[k], i + 1);
    return base;
  };
  return go(obj, 0) as T;
}

export function splitPath(path: string): (string | number)[] {
  const out: (string | number)[] = [];
  for (const m of path.matchAll(/([^.[\]]+)|\[(\d+)\]/g)) {
    if (m[2] !== undefined) out.push(Number(m[2]));
    else if (m[1] !== undefined) out.push(m[1]);
  }
  return out;
}

export function joinPath(prefix: string, key: string | number): string {
  if (typeof key === 'number') return `${prefix}[${key}]`;
  return prefix === '' ? key : `${prefix}.${key}`;
}

/** Whether a secret is stored (`{"set": true}` as the server sends it). */
export function secretIsSet(v: unknown): boolean {
  return typeof v === 'object' && v !== null && (v as { set?: unknown }).set === true;
}

/** Lines of a list field. */
export function linesToList(text: string): string[] {
  return text
    .split('\n')
    .map((s) => s.trim())
    .filter((s) => s !== '');
}

/** `name: value` lines of a map field (a line without `:` is ignored). */
export function linesToMap(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const i = line.indexOf(':');
    if (i <= 0) continue;
    const k = line.slice(0, i).trim();
    if (k !== '') out[k] = line.slice(i + 1).trim();
  }
  return out;
}

export function mapToLines(v: unknown): string {
  if (v === null || typeof v !== 'object') return '';
  return Object.entries(v as Record<string, unknown>)
    .map(([k, x]) => `${k}: ${String(x)}`)
    .join('\n');
}

/** Whether a setting (`sip.bind`) is one that waits for a restart (from `restart_fields`). */
export function needsRestart(restartFields: readonly string[], section: string, path: string): boolean {
  const full = `${section}.${path}`;
  return restartFields.some((f) => f === full || full.startsWith(`${f}.`) || full.startsWith(`${f}[`));
}
