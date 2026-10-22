// The event stream of the signed-in session, shared by the pages.

import { EventStream, eventsUrl, type ServerEvent, type SocketLike } from './events';
import { ended } from './session.svelte';

type Listener = (e: ServerEvent) => void;

const listeners = new Set<Listener>();
const resyncs = new Set<() => void>();
let stream: EventStream | null = null;
let connected = $state(false);

export const live = {
  get connected(): boolean {
    return connected;
  },
};

/** Calls `f` with every event; returns the unsubscribe. */
export function onEvent(f: Listener): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

/** Calls `f` when the events missed cannot be replayed (reload what is shown). */
export function onResync(f: () => void): () => void {
  resyncs.add(f);
  return () => resyncs.delete(f);
}

export function startLive(): void {
  stopLive();
  stream = new EventStream({
    url: (since) => eventsUrl(location, since),
    connect: (url) => new WebSocket(url) as unknown as SocketLike,
    onEvent: (e) => listeners.forEach((f) => f(e)),
    onResync: () => resyncs.forEach((f) => f()),
    onSessionEnded: () => ended(),
    onState: (c) => (connected = c),
  });
  stream.start();
}

export function stopLive(): void {
  stream?.stop();
  stream = null;
  connected = false;
}
