// The client of `/api/v1/events` (spec 4): every message has a `seq`; after a reconnect the
// client asks for what it missed with `since`, and the server answers `resync` when it cannot
// (the client then reloads what it shows).

export interface ServerEvent {
  seq: number;
  type: string;
  at?: string;
  data?: unknown;
}

/** Close codes of the server (spec 4). */
export const CLOSE_SESSION_ENDED = 4001;
export const CLOSE_NO_PONG = 4002;
export const CLOSE_LAGGING = 4008;

export interface SocketLike {
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code: number }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  close(code?: number): void;
}

export interface EventStreamOptions {
  /** `ws(s)://host/api/v1/events` plus the query. */
  url: (since: number | null) => string;
  connect: (url: string) => SocketLike;
  onEvent: (e: ServerEvent) => void;
  /** The server could not replay what was missed: reload. */
  onResync: () => void;
  /** The session ended (4001): show the login. */
  onSessionEnded: () => void;
  onState?: (connected: boolean) => void;
  setTimer?: (f: () => void, ms: number) => unknown;
  clearTimer?: (h: unknown) => void;
  /** Delays between reconnects, the last one repeats. */
  backoff?: number[];
}

/** Delays of the reconnects (ms). */
export const BACKOFF = [500, 1000, 2000, 5000, 10000];

export class EventStream {
  private readonly o: EventStreamOptions;
  private socket: SocketLike | null = null;
  private timer: unknown = null;
  private attempts = 0;
  private stopped = true;
  /** The last `seq` seen (null: never connected). */
  lastSeq: number | null = null;
  connected = false;

  constructor(o: EventStreamOptions) {
    this.o = o;
  }

  start(): void {
    this.stopped = false;
    this.open();
  }

  stop(): void {
    this.stopped = true;
    this.clear();
    const s = this.socket;
    this.socket = null;
    if (s) {
      s.onclose = null;
      s.close(1000);
    }
    this.setConnected(false);
  }

  private clear(): void {
    if (this.timer !== null) (this.o.clearTimer ?? clearTimeout)(this.timer as ReturnType<typeof setTimeout>);
    this.timer = null;
  }

  private setConnected(c: boolean): void {
    if (this.connected !== c) {
      this.connected = c;
      this.o.onState?.(c);
    }
  }

  private open(): void {
    if (this.stopped) return;
    const s = this.o.connect(this.o.url(this.lastSeq));
    this.socket = s;
    s.onopen = () => {
      this.attempts = 0;
      this.setConnected(true);
    };
    s.onmessage = (ev) => this.message(ev.data);
    s.onerror = () => {
      // onclose follows
    };
    s.onclose = (ev) => this.closed(ev.code);
  }

  private message(raw: unknown): void {
    if (typeof raw !== 'string') return;
    let e: ServerEvent;
    try {
      e = JSON.parse(raw) as ServerEvent;
    } catch {
      return;
    }
    if (typeof e.seq !== 'number' || typeof e.type !== 'string') return;
    switch (e.type) {
      case 'hello':
        // the replay (or `resync`) follows; a fresh client starts here
        if (this.lastSeq === null) this.lastSeq = e.seq;
        return;
      case 'resync':
        this.lastSeq = e.seq;
        this.o.onResync();
        return;
      default:
        // duplicates cannot come from the server, but a replay may overlap after a race
        if (this.lastSeq !== null && e.seq <= this.lastSeq) return;
        this.lastSeq = e.seq;
        this.o.onEvent(e);
    }
  }

  private closed(code: number): void {
    this.socket = null;
    this.setConnected(false);
    if (this.stopped) return;
    if (code === CLOSE_SESSION_ENDED) {
      this.stopped = true;
      this.o.onSessionEnded();
      return;
    }
    const backoff = this.o.backoff ?? BACKOFF;
    // lagging behind: the server dropped us on purpose, catch up at once
    const delay = code === CLOSE_LAGGING ? 0 : (backoff[Math.min(this.attempts, backoff.length - 1)] ?? 1000);
    this.attempts += 1;
    this.timer = (this.o.setTimer ?? setTimeout)(() => {
      this.timer = null;
      this.open();
    }, delay);
  }
}

/** The URL of the events of this page's server. */
export function eventsUrl(loc: { protocol: string; host: string }, since: number | null): string {
  const scheme = loc.protocol === 'https:' ? 'wss:' : 'ws:';
  const q = since === null ? '' : `?since=${since}`;
  return `${scheme}//${loc.host}/api/v1/events${q}`;
}
