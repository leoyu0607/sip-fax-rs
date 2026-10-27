<script lang="ts">
  import { onMount } from 'svelte';
  import { get, setOnUnauthenticated, type Status } from './lib/api';
  import { i18n, LOCALES, t, type Locale } from './lib/i18n.svelte';
  import { faxList } from './lib/faxlist.svelte';
  import { live, onEvent, onResync, startLive, stopLive } from './lib/live.svelte';
  import { ADMIN_PAGES, href } from './lib/route';
  import { setTheme, storedTheme, THEMES, type Theme } from './lib/theme';
  import { unread } from './lib/unread.svelte';
  import { ended, logout, restore, router, session, storeLocale } from './lib/session.svelte';
  import Login from './routes/Login.svelte';
  import ChangePassword from './routes/ChangePassword.svelte';
  import FaxList from './routes/FaxList.svelte';
  import FaxDetail from './routes/FaxDetail.svelte';
  import Settings from './routes/Settings.svelte';
  import Users from './routes/Users.svelte';
  import ApiKeys from './routes/ApiKeys.svelte';
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

  // the live connection: nothing to see while it works; a notice when it has been down for a
  // few seconds (not at every short reconnect)
  let liveDown = $state(false);
  $effect(() => {
    if (live.connected || !signedIn) {
      liveDown = false;
      return;
    }
    const timer = setTimeout(() => (liveDown = true), 3000);
    return () => clearTimeout(timer);
  });

  // the unread badge of "Faxes"
  $effect(() => {
    if (!signedIn) {
      unread.reset();
      return;
    }
    void unread.refresh();
    const offEvent = onEvent((e) => unread.event(e));
    const offResync = onResync(() => void unread.refresh());
    return () => {
      offEvent();
      offResync();
    };
  });

  // the account menu (account page, language, theme, sign out): closes on a click outside it,
  // Escape, or when a page is chosen in it. The listeners stay while someone is signed in and look
  // at the element itself: `bind:open` follows the `toggle` event, which comes a moment after the
  // click that opened it, and a click outside within that moment was once missed.
  let menu = $state<HTMLDetailsElement | null>(null);
  function closeMenu() {
    if (menu) menu.open = false;
  }
  $effect(() => {
    if (!signedIn) return;
    const outside = (e: MouseEvent) => {
      if (menu?.open && !menu.contains(e.target as Node)) closeMenu();
    };
    const escape = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && menu?.open) {
        closeMenu();
        menu.querySelector('summary')?.focus();
      }
    };
    document.addEventListener('click', outside);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('click', outside);
      document.removeEventListener('keydown', escape);
    };
  });

  // no trunk yet (a new installation): every call is refused, say so on every page
  let trunks = $state<number | null>(null);
  // no address to advertise (ADR-071): calls work through the address towards each peer, but
  // behind NAT or with several networks that guess is wrong; the administrator is asked to set it
  let advertisedMissing = $state(false);
  $effect(() => {
    if (!signedIn) {
      trunks = null;
      advertisedMissing = false;
      return;
    }
    get<Status>('/status')
      .then((s) => {
        trunks = s.service.trunks.length;
        advertisedMissing = s.service.advertised_missing === true;
      })
      .catch(() => undefined);
    return onEvent((e) => {
      const d = e.data as { trunks?: unknown[]; advertised_missing?: boolean } | undefined;
      if (e.type === 'status.changed' && Array.isArray(d?.trunks)) {
        trunks = d.trunks.length;
        advertisedMissing = d.advertised_missing === true;
      }
    });
  });

  const page = $derived(router.route.page);
  const forbidden = $derived(ADMIN_PAGES.has(page) && !session.isAdmin);

  let theme = $state<Theme>(storedTheme());
  function chooseTheme(e: Event) {
    theme = (e.currentTarget as HTMLSelectElement).value as Theme;
    setTheme(theme);
  }

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
            { page: 'apikeys', label: 'nav.apikeys', link: href({ page: 'apikeys' }) },
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
          {@const badge = n.page === 'faxes' && unread.count ? unread.count : 0}
          <li>
            <!-- the link's name stays "Faxes"; the count is its description -->
            <a href={n.link} aria-current={current(n.page) ? 'page' : undefined} aria-describedby={badge ? 'unread-count' : undefined}
              >{t(n.label)}{#if badge}<span class="count" aria-hidden="true" title={t('nav.unread', { count: badge })}>{badge > 99 ? '99+' : badge}</span>{/if}</a
            >
            {#if badge}<span id="unread-count" class="sr-only">{t('nav.unread', { count: badge })}</span>{/if}
          </li>
        {/each}
      </ul>
    </nav>
    <div class="tools">
      {#if liveDown}
        <span class="live off" role="status"><span aria-hidden="true">⚠</span> {t('live.off')}</span>
      {:else}
        <span class="sr-only" role="status">{t(live.connected ? 'live.on' : 'live.connecting')}</span>
      {/if}
      <details class="user-menu" bind:this={menu}>
        <summary aria-label={t('nav.menu_of', { name: session.user.display_name ?? session.user.username })}>
          <span class="avatar" aria-hidden="true">{(session.user.display_name ?? session.user.username).slice(0, 1).toUpperCase()}</span>
          <span class="name">{session.user.display_name ?? session.user.username}</span>
          <span class="caret" aria-hidden="true">▾</span>
        </summary>
        <div class="menu-panel">
          <a href="#/account" onclick={closeMenu}>{t('account.title')}</a>
          <div class="menu-field">
            <label for="locale">{t('nav.language')}</label>
            <select id="locale" value={i18n.locale} onchange={setLocale}>
              {#each LOCALES as l (l)}
                <option value={l}>{t(`locale.${l}`)}</option>
              {/each}
            </select>
          </div>
          <div class="menu-field">
            <label for="theme">{t('nav.theme')}</label>
            <select id="theme" value={theme} onchange={chooseTheme}>
              {#each THEMES as th (th)}
                <option value={th}>{t(`theme.${th}`)}</option>
              {/each}
            </select>
          </div>
          <button type="button" class="menu-item" onclick={() => logout()}>{t('nav.logout')}</button>
        </div>
      </details>
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
  {#if advertisedMissing && session.isAdmin}
    <div class="notice restart setup-banner advertised-banner" role="status">
      <span aria-hidden="true">⚠</span>
      {t('setup.advertised_missing')}
      <a href={href({ page: 'settings', section: 'sip' })}>{t('setup.go_sip')}</a>
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
    {:else if page === 'apikeys'}
      <ApiKeys />
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
