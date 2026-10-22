import { describe, expect, it } from 'vitest';
import { CLOSE_LAGGING, CLOSE_SESSION_ENDED, EventStream, eventsUrl, type ServerEvent, type SocketLike } from './events';

class FakeSocket implements SocketLike {
  onopen: SocketLike['onopen'] = null;
  onmessage: SocketLike['onmessage'] = null;
  onclose: SocketLike['onclose'] = null;
  onerror: SocketLike['onerror'] = null;
  closed: number | null = null;
  constructor(readonly url: string) {}
  close(code?: number) {
    this.closed = code ?? 1000;
  }
  send(o: unknown) {
    this.onmessage?.({ data: typeof o === 'string' ? o : JSON.stringify(o) });
  }
}

function harness() {
  const sockets: FakeSocket[] = [];
  const events: ServerEvent[] = [];
  const timers: { f: () => void; ms: number }[] = [];
  let resyncs = 0;
  let ended = 0;
  const states: boolean[] = [];
  const s = new EventStream({
    url: (since) => eventsUrl({ protocol: 'https:', host: 'fax.example:8443' }, since),
    connect: (url) => {
      const k = new FakeSocket(url);
      sockets.push(k);
      return k;
    },
    onEvent: (e) => events.push(e),
    onResync: () => (resyncs += 1),
    onSessionEnded: () => (ended += 1),
    onState: (c) => states.push(c),
    setTimer: (f, ms) => {
      timers.push({ f, ms });
      return timers.length;
    },
    clearTimer: () => {},
    backoff: [100, 200],
  });
  return { s, sockets, events, timers, states, get resyncs() { return resyncs; }, get ended() { return ended; } };
}

describe('U04 event stream', () => {
  it('reconnects with since and replays only what is new', () => {
    const h = harness();
    h.s.start();
    const a = h.sockets[0]!;
    expect(a.url).toBe('wss://fax.example:8443/api/v1/events');
    a.onopen?.({});
    a.send({ seq: 5, type: 'hello' });
    a.send({ seq: 6, type: 'fax.created', data: { id: 1 } });
    a.send({ seq: 7, type: 'fax.progress', data: { id: 1, pages: 1 } });
    a.onclose?.({ code: 1006 });
    expect(h.states).toEqual([true, false]);
    expect(h.timers.map((t) => t.ms)).toEqual([100]);
    h.timers[0]!.f();
    const b = h.sockets[1]!;
    expect(b.url).toBe('wss://fax.example:8443/api/v1/events?since=7');
    b.onopen?.({});
    b.send({ seq: 9, type: 'hello' });
    b.send({ seq: 7, type: 'fax.progress', data: { id: 1, pages: 1 } }); // overlap: dropped
    b.send({ seq: 8, type: 'fax.finalized', data: { id: 1 } });
    b.send('not json');
    b.send({ type: 'no seq' });
    expect(h.events.map((e) => e.seq)).toEqual([6, 7, 8]);
    expect(h.resyncs).toBe(0);
  });

  it('resync asks for a reload and continues from its seq', () => {
    const h = harness();
    h.s.start();
    const a = h.sockets[0]!;
    a.send({ seq: 3, type: 'hello' });
    a.send({ seq: 4, type: 'x' });
    a.onclose?.({ code: 1006 });
    h.timers[0]!.f();
    const b = h.sockets[1]!;
    b.send({ seq: 1, type: 'hello' });
    b.send({ seq: 1, type: 'resync' }); // the server restarted
    expect(h.resyncs).toBe(1);
    b.send({ seq: 2, type: 'fax.created', data: { id: 9 } });
    expect(h.events.map((e) => e.seq)).toEqual([4, 2]);
  });

  it('backs off, catches up at once when lagging, stops on 4001', () => {
    const h = harness();
    h.s.start();
    h.sockets[0]!.onclose?.({ code: 1006 });
    h.timers[0]!.f();
    h.sockets[1]!.onclose?.({ code: 1006 });
    h.timers[1]!.f();
    h.sockets[2]!.onclose?.({ code: 1006 });
    expect(h.timers.map((t) => t.ms)).toEqual([100, 200, 200]);
    h.timers[2]!.f();
    h.sockets[3]!.onopen?.({});
    h.sockets[3]!.onclose?.({ code: CLOSE_LAGGING });
    expect(h.timers[3]!.ms).toBe(0);
    h.timers[3]!.f();
    h.sockets[4]!.onclose?.({ code: CLOSE_SESSION_ENDED });
    expect(h.ended).toBe(1);
    expect(h.timers.length).toBe(4);
  });

  it('stop closes and does not reconnect', () => {
    const h = harness();
    h.s.start();
    h.sockets[0]!.onopen?.({});
    h.s.stop();
    expect(h.sockets[0]!.closed).toBe(1000);
    expect(h.timers.length).toBe(0);
    expect(h.s.connected).toBe(false);
    expect(eventsUrl({ protocol: 'http:', host: 'h' }, 3)).toBe('ws://h/api/v1/events?since=3');
  });
});
