// Hash routes (`#/faxes/12`): the server only has to serve `/` and the assets.

export type Route =
  | { page: 'faxes' }
  | { page: 'fax'; id: number }
  | { page: 'settings'; section: string | null }
  | { page: 'users' }
  | { page: 'audit' }
  | { page: 'status' }
  | { page: 'account' }
  | { page: 'not_found' };

/** The pages only an administrator sees (spec 2.1). */
export const ADMIN_PAGES: ReadonlySet<Route['page']> = new Set(['settings', 'users', 'audit']);

export function parseRoute(hash: string): Route {
  const path = hash.replace(/^#/, '').replace(/^\/+/, '').replace(/\/+$/, '');
  const [head, ...rest] = path === '' ? [] : path.split('/').map(decodeURIComponent);
  switch (head) {
    case undefined:
    case 'faxes':
      if (rest.length === 0) return { page: 'faxes' };
      if (rest.length === 1 && /^[1-9]\d*$/.test(rest[0] ?? '')) return { page: 'fax', id: Number(rest[0]) };
      return { page: 'not_found' };
    case 'settings':
      if (rest.length === 0) return { page: 'settings', section: null };
      if (rest.length === 1) return { page: 'settings', section: rest[0] ?? null };
      return { page: 'not_found' };
    case 'users':
    case 'audit':
    case 'status':
    case 'account':
      return rest.length === 0 ? { page: head } : { page: 'not_found' };
    default:
      return { page: 'not_found' };
  }
}

export function href(r: Route): string {
  switch (r.page) {
    case 'fax':
      return `#/faxes/${r.id}`;
    case 'settings':
      return r.section === null ? '#/settings' : `#/settings/${encodeURIComponent(r.section)}`;
    case 'not_found':
      return '#/';
    default:
      return `#/${r.page}`;
  }
}
