<script lang="ts">
  import { onMount } from 'svelte';
  import { get, setOnUnauthenticated, type Status } from './lib/api';
  import { i18n, LOCALES, t, type Locale } from './lib/i18n.svelte';
  import { faxList } from './lib/faxlist.svelte';
  import { live, onEvent, onResync, startLive, stopLive } from './lib/live.svelte';
  import { ADMIN_PAGES, href } from './lib/route';
  import { ended, logout, restore, router, session, storeLocale } from './lib/session.svelte';
  import Login from './routes/Login.svelte';
  import ChangePassword from './routes/ChangePassword.svelte';
  import FaxList from './routes/FaxList.svelte';
  import FaxDetail from './routes/FaxDetail.svelte';
  import Settings from './routes/Settings.svelte';
  import Users from './routes/Users.svelte';
  import Audit from './routes/Audit.svelte';
  import StatusPage from './routes/Status.svelte';
  import Account from './routes/Account.svelte';

  let loadError = $state(false);

  onMount(() => {
    setOnUnauthenticated(() => ended());
    restore().catch(() => (loadError = true));
  });

  // the event stream runs while someone is signed in (and past the forced password change)
  const signedIn = $derived(session.user !== null && !session.user.must_change_password);
  $effect(() => {
    if (signedIn) startLive();
    else stopLive();
    return () => stopLive();
  });

  // The fax list follows the events while someone is signed in, not only while its page is
  // open: coming back to it shows what arrived meanwhile (new faxes wait behind the banner).
  $effect(() => {
    if (!signedIn) return;
    faxList.loaded = false;
    const offEvent = onEvent((e) => {
      if (e.type.startsWith('fax.')) faxList.event(e, router.route.page !== 'faxes' || window.scrollY > 80);
    });
    const offResync = onResync(() => {
      if (faxList.loaded) faxList.reload();
    });
    return () => {
      offEvent();
      offResync();
    };
  });

  // no trunk yet (a new installation): every call is refused, say so on every page
  let trunks = $state<number | null>(null);
  $effect(() => {
    if (!signedIn) {
      trunks = null;
      return;
    }
    get<Status>('/status')
      .then((s) => (trunks = s.service.trunks.length))
      .catch(() => undefined);
    return onEvent((e) => {
      const d = e.data as { trunks?: unknown[] } | undefined;
      if (e.type === 'status.changed' && Array.isArray(d?.trunks)) trunks = d.trunks.length;
    });
  });

  const page = $derived(router.route.page);
  const forbidden = $derived(ADMIN_PAGES.has(page) && !session.isAdmin);

  function setLocale(e: Event) {
    const l = (e.currentTarget as HTMLSelectElement).value as Locale;
    i18n.set(l);
    storeLocale(l);
  }

  const nav = $derived(
    [
      { page: 'faxes', label: 'nav.faxes', link: href({ page: 'faxes' }) },
      { page: 'status', label: 'nav.status', link: href({ page: 'status' }) },
      ...(session.isAdmin
        ? [
            { page: 'settings', label: 'nav.settings', link: href({ page: 'settings', section: null }) },
            { page: 'users', label: 'nav.users', link: href({ page: 'users' }) },
            { page: 'audit', label: 'nav.audit', link: href({ page: 'audit' }) },
          ]
        : []),
    ] as const,
  );

  function current(p: string): boolean {
    return p === page || (p === 'faxes' && page === 'fax');
  }
</script>

<svelte:head>
  <title>{t('app.title')}</title>
</svelte:head>

{#if loadError}
  <main class="center"><p role="alert">{t('error.unavailable')}</p></main>
{:else if !session.ready}
  <main class="center"><p>{t('app.loading')}</p></main>
{:else if session.user === null}
  <Login />
{:else if session.user.must_change_password}
  <ChangePassword forced />
{:else}
  <header class="topbar">
    <a class="brand" href="#/faxes">{t('app.title')}</a>
    <nav aria-label={t('nav.main')}>
      <ul>
        {#each nav as n (n.page)}
          <li>
            <a href={n.link} aria-current={current(n.page) ? 'page' : undefined}>{t(n.label)}</a>
          </li>
        {/each}
      </ul>
    </nav>
    <div class="tools">
      <span class="live" class:off={!live.connected} title={t(live.connected ? 'live.on' : 'live.off')}>
        <span aria-hidden="true">{live.connected ? '●' : '○'}</span>
        <span class="sr-only">{t(live.connected ? 'live.on' : 'live.off')}</span>
      </span>
      <label class="sr-only" for="locale">{t('nav.language')}</label>
      <select id="locale" value={i18n.locale} onchange={setLocale}>
        {#each LOCALES as l (l)}
          <option value={l}>{t(`locale.${l}`)}</option>
        {/each}
      </select>
      <a href="#/account">{session.user.display_name ?? session.user.username}</a>
      <button type="button" class="link" onclick={() => logout()}>{t('nav.logout')}</button>
    </div>
  </header>
  {#if trunks === 0}
    <div class="notice restart setup-banner" role="alert">
      <span aria-hidden="true">⚠</span>
      {#if session.isAdmin}
        {t('setup.no_trunk_admin')}
        <a href={href({ page: 'settings', section: 'trunks' })}>{t('setup.go_trunks')}</a>
      {:else}
        {t('setup.no_trunk_agent')}
      {/if}
    </div>
  {/if}
  <main id="main">
    {#if forbidden}
      <section class="page"><h1>{t('error.forbidden')}</h1></section>
    {:else if page === 'faxes'}
      <FaxList />
    {:else if router.route.page === 'fax'}
      {#key router.route.id}
        <FaxDetail id={router.route.id} />
      {/key}
    {:else if router.route.page === 'settings'}
      <Settings section={router.route.section} />
    {:else if page === 'users'}
      <Users />
    {:else if page === 'audit'}
      <Audit />
    {:else if page === 'status'}
      <StatusPage />
    {:else if page === 'account'}
      <Account />
    {:else}
      <section class="page"><h1>{t('error.not_found')}</h1></section>
    {/if}
  </main>
{/if}
