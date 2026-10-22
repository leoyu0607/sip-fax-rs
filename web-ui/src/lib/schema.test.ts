import { describe, expect, it } from 'vitest';
import {
  DELIVERY_KINDS, getPath, joinPath, linesToList, linesToMap, mapToLines, needsRestart, newDelivery, newRoute,
  newTrunk, secretIsSet, setPath, splitPath,
} from './schema';

describe('schema helpers', () => {
  it('paths', () => {
    expect(splitPath('routes[1].deliveries[0].url')).toEqual(['routes', 1, 'deliveries', 0, 'url']);
    expect(joinPath('', 'a')).toBe('a');
    expect(joinPath('a', 'b')).toBe('a.b');
    expect(joinPath('a', 2)).toBe('a[2]');
    const body = { t38: { max_version: 0 }, routes: [{ deliveries: [{ url: 'x' }] }] };
    expect(getPath(body, 't38.max_version')).toBe(0);
    expect(getPath(body, 'routes[0].deliveries[0].url')).toBe('x');
    expect(getPath(body, 'nope.deeper')).toBeUndefined();
    const next = setPath(body, 'routes[0].deliveries[0].url', 'y');
    expect(getPath(next, 'routes[0].deliveries[0].url')).toBe('y');
    expect(getPath(body, 'routes[0].deliveries[0].url')).toBe('x'); // not changed
    expect(next.t38).toBe(body.t38); // untouched branches shared
    expect(setPath({}, 'a[1].b', 1)).toEqual({ a: [undefined, { b: 1 }] });
  });

  it('lists, maps, secrets', () => {
    expect(linesToList(' a \n\n b\n')).toEqual(['a', 'b']);
    expect(linesToMap('X-A: 1\nbad\n: x\nX-B: a:b')).toEqual({ 'X-A': '1', 'X-B': 'a:b' });
    expect(mapToLines({ a: '1', b: 2 })).toBe('a: 1\nb: 2');
    expect(mapToLines(null)).toBe('');
    expect(secretIsSet({ set: true, ref: 'x' })).toBe(true);
    expect(secretIsSet({ set: false })).toBe(false);
    expect(secretIsSet('plain')).toBe(false);
  });

  it('restart marks', () => {
    const r = ['sip.bind', 'capture'];
    expect(needsRestart(r, 'sip', 'bind')).toBe(true);
    expect(needsRestart(r, 'sip', 'user_agent')).toBe(false);
    expect(needsRestart(r, 'capture', 'dir')).toBe(true);
    expect(needsRestart(r, 'media', 'port_range')).toBe(false);
  });

  it('new values', () => {
    for (const k of DELIVERY_KINDS) expect(newDelivery(k).kind).toBe(k);
    expect(newRoute().deliveries).toEqual([]);
    expect(newTrunk('gw')).toEqual({ name: 'gw', preset: 'generic', peer: '' });
  });
});
