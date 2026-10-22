<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { put, type Fax } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { EMPTY_FILTER, FAX_STATUSES, isEmpty } from '../lib/faxes';
  import { faxList } from '../lib/faxlist.svelte';
  import { fmt, t } from '../lib/i18n.svelte';
  import FaxStatus from '../components/FaxStatus.svelte';

  let sentinel = $state<HTMLElement | null>(null);
  let rowError = $state<string | null>(null);
  let showFilters = $state(!isEmpty(faxList.filter));

  // the form edits a copy; the list changes on submit
  let draft = $state({ ...faxList.filter });

  onMount(() => {
    if (!faxList.loaded) faxList.reload();
    else tick().then(() => window.scrollTo(0, faxList.scrollY));
    // (the events are followed by App.svelte, also while another page is open)
    return () => {
      faxList.scrollY = window.scrollY;
    };
  });

  // keyset "infinite" scroll: the next page loads when the end of the list comes into view
  $effect(() => {
    const s = sentinel;
    if (!s || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((x) => x.isIntersecting)) faxList.more();
    });
    io.observe(s);
    return () => io.disconnect();
  });

  function apply(e: SubmitEvent) {
    e.preventDefault();
    faxList.filter = { ...draft };
    faxList.reload();
  }

  function clear() {
    draft = { ...EMPTY_FILTER };
    faxList.filter = { ...EMPTY_FILTER };
    faxList.reload();
  }

  function quickRead(v: 'all' | 'unread' | 'read') {
    draft.read = v;
    faxList.filter = { ...faxList.filter, read: v };
    faxList.reload();
  }

  async function toggleRead(f: Fax) {
    rowError = null;
    try {
      faxList.replace(await put<Fax>(`/faxes/${f.id}/read`, { read: !f.is_read }));
    } catch (e) {
      rowError = errorText(e);
    }
  }

  function showNew() {
    faxList.showPending();
    window.scrollTo(0, 0);
  }
</script>

<section class="page" aria-labelledby="faxes-title">
  <div class="page-head">
    <h1 id="faxes-title">{t('faxes.title')}</h1>
    <div class="segmented" role="group" aria-label={t('faxes.filter.read')}>
      {#each ['all', 'unread', 'read'] as const as v (v)}
        <button type="button" aria-pressed={faxList.filter.read === v} onclick={() => quickRead(v)}>{t(`faxes.read.${v}`)}</button>
      {/each}
    </div>
    <button type="button" aria-expanded={showFilters} aria-controls="fax-filters" onclick={() => (showFilters = !showFilters)}>
      {t('faxes.filter.toggle')}
    </button>
  </div>

  {#if showFilters}
    <form id="fax-filters" class="filters" onsubmit={apply}>
      <div class="field">
        <label for="f-q">{t('faxes.filter.q')}</label>
        <input id="f-q" type="search" bind:value={draft.q} />
      </div>
      <div class="field">
        <label for="f-status">{t('faxes.filter.status')}</label>
        <select id="f-status" bind:value={draft.status}>
          <option value="">{t('faxes.filter.any')}</option>
          {#each FAX_STATUSES as s (s)}<option value={s}>{t(`fax.status.${s}`)}</option>{/each}
        </select>
      </div>
      <div class="field">
        <label for="f-from">{t('faxes.filter.from')}</label>
        <input id="f-from" type="date" bind:value={draft.from} />
      </div>
      <div class="field">
        <label for="f-to">{t('faxes.filter.to')}</label>
        <input id="f-to" type="date" bind:value={draft.to} />
      </div>
      <div class="field">
        <label for="f-ani">{t('faxes.filter.ani')}</label>
        <input id="f-ani" inputmode="tel" bind:value={draft.ani} />
      </div>
      <div class="field">
        <label for="f-dnis">{t('faxes.filter.dnis')}</label>
        <input id="f-dnis" inputmode="tel" bind:value={draft.dnis} />
      </div>
      <div class="field">
        <label for="f-trunk">{t('faxes.filter.trunk')}</label>
        <input id="f-trunk" bind:value={draft.trunk} />
      </div>
      <div class="actions">
        <button type="submit" class="primary">{t('faxes.filter.apply')}</button>
        <button type="button" onclick={clear}>{t('faxes.filter.clear')}</button>
      </div>
    </form>
  {/if}

  {#if faxList.pending.length > 0}
    <button type="button" class="banner" onclick={showNew}>
      {t('faxes.new', { count: faxList.pending.length })}
    </button>
  {/if}

  {#if rowError}<p class="error" role="alert">{rowError}</p>{/if}

  <div class="table-wrap">
    <table class="faxes">
      <caption class="sr-only">{t('faxes.title')}</caption>
      <thead>
        <tr>
          <th scope="col">{t('fax.started_at')}</th>
          <th scope="col">{t('fax.ani')}</th>
          <th scope="col">{t('fax.dnis')}</th>
          <th scope="col">{t('fax.status_label')}</th>
          <th scope="col" class="num">{t('fax.pages')}</th>
          <th scope="col">{t('fax.trunk')}</th>
          <th scope="col">{t('fax.note')}</th>
          <th scope="col"><span class="sr-only">{t('fax.read_label')}</span></th>
        </tr>
      </thead>
      <tbody>
        {#each faxList.items as f (f.id)}
          <tr class:unread={!f.is_read} data-fax-id={f.id}>
            <td>
              <a href="#/faxes/{f.id}">{fmt(f.started_at)}{#if !f.is_read}<span class="sr-only"> {t('fax.unread')}</span>{/if}</a>
            </td>
            <td>{f.ani ?? t('fax.unknown_number')}</td>
            <td>{f.dnis ?? ''}</td>
            <td><FaxStatus fax={f} /></td>
            <td class="num">{f.pages}</td>
            <td>{f.trunk ?? ''}</td>
            <td class="note-cell">{f.note ?? ''}</td>
            <td>
              <button type="button" class="small" onclick={() => toggleRead(f)} aria-label={t(f.is_read ? 'fax.mark_unread_of' : 'fax.mark_read_of', { time: fmt(f.started_at) })}>
                {t(f.is_read ? 'fax.mark_unread' : 'fax.mark_read')}
              </button>
            </td>
          </tr>
        {:else}
          {#if faxList.loaded && !faxList.loading}
            <tr><td colspan="8" class="empty">{t('faxes.empty')}</td></tr>
          {/if}
        {/each}
      </tbody>
    </table>
  </div>

  {#if faxList.error}
    <p class="error" role="alert">{errorText(faxList.error)}
      <button type="button" onclick={() => (faxList.loaded ? faxList.more() : faxList.reload())}>{t('common.retry')}</button>
    </p>
  {/if}
  {#if faxList.loading}<p class="muted" role="status">{t('app.loading')}</p>{/if}
  {#if faxList.cursor !== null && !faxList.loading}
    <div bind:this={sentinel} class="more">
      <button type="button" onclick={() => faxList.more()}>{t('faxes.more')}</button>
    </div>
  {/if}
</section>
