<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError, BASE, get, post, put, type Delivery, type Fax } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { faxList } from '../lib/faxlist.svelte';
  import { hangupKey } from '../lib/faxes';
  import { fmt, formatBytes, formatDuration, hasKey, t } from '../lib/i18n.svelte';
  import { onEvent, onResync } from '../lib/live.svelte';
  import { session } from '../lib/session.svelte';
  import FaxStatus from '../components/FaxStatus.svelte';

  let { id }: { id: number } = $props();

  let fax = $state<Fax | null>(null);
  let loadError = $state<unknown>(null);
  let actionError = $state<string | null>(null);
  let page = $state(1);
  let zoom = $state<'fit' | number>('fit');
  let natural = $state(0);

  // the note: what the user types, and the version it was started from
  let note = $state('');
  let noteBase = $state<string | null>(null);
  let noteSaving = $state(false);
  let noteSaved = $state(false);
  let conflict = $state<{ note: string | null; by: string | null; at: string | null } | null>(null);
  const noteDirty = $derived(fax !== null && note !== (fax.note ?? ''));

  let redelivering = $state(false);
  let redelivered = $state<number | null>(null);

  const ZOOMS = [0.5, 0.75, 1, 1.5, 2];
  const imgWidth = $derived(zoom === 'fit' || natural === 0 ? undefined : Math.round(natural * zoom));
  const previews = $derived(fax !== null && fax.has_tiff && fax.pages > 0 && fax.fax_status !== 'receiving');
  const canRedeliver = $derived(
    session.isAdmin && (fax?.deliveries ?? []).some((d) => d.status === 'failed' || d.status === 'giving_up'),
  );

  async function load(keepNote = false) {
    try {
      const f = await get<Fax>(`/faxes/${id}`);
      fax = f;
      loadError = null;
      if (!keepNote || !noteDirty) {
        note = f.note ?? '';
        noteBase = f.note_updated_at;
      }
      if (page > Math.max(1, f.pages)) page = 1;
    } catch (e) {
      loadError = e;
    }
  }

  onMount(() => {
    load();
    const off = onEvent((e) => {
      const d = (e.data ?? {}) as Record<string, unknown>;
      const target = e.type === 'delivery.updated' ? d.fax_id : d.id;
      if (target !== id || fax === null) return;
      switch (e.type) {
        case 'fax.progress':
          if (typeof d.pages === 'number') fax = { ...fax, pages: Math.max(fax.pages, d.pages) };
          return;
        case 'fax.read_changed':
          fax = { ...fax, is_read: Boolean(d.is_read), read_changed_by: (d.by as string) ?? null, read_changed_at: (d.at as string) ?? null };
          return;
        case 'fax.note_changed':
          // someone else saved: take it unless the user is typing (then saving shows the conflict).
          // (dirty is judged before `fax` changes: afterwards every saved note looks edited)
          {
            const typing = noteDirty;
            fax = { ...fax, note: (d.note as string | null) ?? null, note_updated_by: (d.by as string) ?? null, note_updated_at: (d.at as string) ?? null };
            if (!typing) {
              note = fax.note ?? '';
              noteBase = fax.note_updated_at;
            }
          }
          return;
        default:
          load(true);
      }
    });
    const offResync = onResync(() => load(true));
    return () => {
      off();
      offResync();
    };
  });

  function changed(f: Fax) {
    fax = { ...f, deliveries: f.deliveries ?? fax?.deliveries };
    faxList.replace(fax);
  }

  async function toggleRead() {
    if (!fax) return;
    actionError = null;
    try {
      changed(await put<Fax>(`/faxes/${id}/read`, { read: !fax.is_read }));
    } catch (e) {
      actionError = errorText(e);
    }
  }

  async function saveNote(base: string | null = noteBase) {
    noteSaving = true;
    noteSaved = false;
    actionError = null;
    try {
      const f = await put<Fax>(`/faxes/${id}/note`, { text: note, expected_updated_at: base });
      changed(f);
      noteBase = f.note_updated_at;
      note = f.note ?? '';
      conflict = null;
      noteSaved = true;
    } catch (e) {
      if (e instanceof ApiError && e.code === 'conflict' && e.fields) {
        conflict = {
          note: (e.fields.note as string | null) ?? null,
          by: (e.fields.note_updated_by as string | null) ?? null,
          at: (e.fields.note_updated_at as string | null) ?? null,
        };
      } else {
        actionError = errorText(e);
      }
    } finally {
      noteSaving = false;
    }
  }

  function takeTheirs() {
    if (!conflict) return;
    note = conflict.note ?? '';
    noteBase = conflict.at;
    conflict = null;
  }

  function keepMine() {
    if (!conflict) return;
    const base = conflict.at;
    saveNote(base);
  }

  async function redeliver() {
    redelivering = true;
    actionError = null;
    try {
      const r = await post<{ queued: unknown[]; deliveries: Delivery[] }>(`/faxes/${id}/redeliver`, {});
      redelivered = r.queued.length;
      if (fax) fax = { ...fax, deliveries: r.deliveries };
    } catch (e) {
      actionError = errorText(e);
    } finally {
      redelivering = false;
    }
  }

  function pngUrl(n: number, kind: 'full' | 'thumb'): string {
    return `${BASE}/faxes/${id}/pages/${n}.png?kind=${kind}`;
  }

  function hangup(f: Fax): string {
    const k = hangupKey(f.hangup_code);
    if (k === null) return '';
    return hasKey(k) ? t(k) : f.hangup_code ?? '';
  }

  function onKey(e: KeyboardEvent) {
    if (!fax || !previews) return;
    const el = e.target as HTMLElement | null;
    if (el && (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT' || el.tagName === 'SELECT')) return;
    if (e.key === 'ArrowRight' || e.key === 'PageDown') page = Math.min(fax.pages, page + 1);
    else if (e.key === 'ArrowLeft' || e.key === 'PageUp') page = Math.max(1, page - 1);
  }
</script>

<svelte:window onkeydown={onKey} />

<section class="page fax-detail" aria-labelledby="fax-title">
  <p><a href="#/faxes">← {t('fax.back')}</a></p>
  {#if loadError}
    <p class="error" role="alert">{errorText(loadError)}</p>
  {:else if fax === null}
    <p class="muted">{t('app.loading')}</p>
  {:else}
    <div class="page-head">
      <h1 id="fax-title">{t('fax.title', { ani: fax.ani ?? t('fax.unknown_number') })}</h1>
      <FaxStatus {fax} />
      <span class="badge" class:unread={!fax.is_read}>
        <span aria-hidden="true">{fax.is_read ? '✉︎' : '●'}</span>
        {t(fax.is_read ? 'fax.read' : 'fax.unread')}
      </span>
    </div>

    <div class="toolbar">
      <button type="button" onclick={toggleRead}>{t(fax.is_read ? 'fax.mark_unread' : 'fax.mark_read')}</button>
      {#if fax.has_pdf}<a class="button" href="{BASE}/faxes/{id}/file.pdf" download>{t('fax.download_pdf')}</a>{/if}
      {#if fax.has_tiff}<a class="button" href="{BASE}/faxes/{id}/file.tif" download>{t('fax.download_tiff')}</a>{/if}
      {#if canRedeliver}
        <button type="button" onclick={redeliver} disabled={redelivering}>{t('fax.redeliver')}</button>
      {/if}
    </div>
    {#if redelivered !== null}<p class="ok" role="status">{t('fax.redelivered', { count: redelivered })}</p>{/if}
    {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

    {#if fax.files_purged_at}
      <p class="notice" role="note">{t('fax.purged', { at: fmt(fax.files_purged_at) })}</p>
    {:else if fax.fax_status === 'receiving'}
      <p class="notice" role="status">{t('fax.receiving_page', { page: fax.pages + 1 })}</p>
    {:else if !previews}
      <p class="notice">{t('fax.no_pages')}</p>
    {/if}

    {#if previews}
      <div class="viewer">
        <ol class="thumbs" aria-label={t('fax.pages_list')}>
          {#each Array.from({ length: fax.pages }, (_, i) => i + 1) as n (n)}
            <li>
              <button type="button" class:current={n === page} aria-current={n === page ? 'true' : undefined} onclick={() => (page = n)}>
                <img src={pngUrl(n, 'thumb')} alt={t('fax.page_n', { page: n })} loading="lazy" />
                <span>{n}</span>
              </button>
            </li>
          {/each}
        </ol>
        <div class="pageview">
          <div class="toolbar">
            <button type="button" onclick={() => (page = Math.max(1, page - 1))} disabled={page <= 1}>{t('fax.prev_page')}</button>
            <span>{t('fax.page_of', { page, pages: fax.pages })}</span>
            <button type="button" onclick={() => (page = Math.min(fax!.pages, page + 1))} disabled={page >= fax.pages}>{t('fax.next_page')}</button>
            <label for="zoom">{t('fax.zoom')}</label>
            <select id="zoom" bind:value={zoom}>
              <option value="fit">{t('fax.zoom_fit')}</option>
              {#each ZOOMS as z (z)}<option value={z}>{Math.round(z * 100)}%</option>{/each}
            </select>
          </div>
          <div class="scroller">
            <img
              class:fit={zoom === 'fit'}
              src={pngUrl(page, 'full')}
              width={imgWidth}
              alt={t('fax.page_n', { page })}
              onload={(e) => (natural = (e.currentTarget as HTMLImageElement).naturalWidth)}
            />
          </div>
        </div>
      </div>
    {/if}

    <div class="columns">
      <section aria-labelledby="note-title" class="card">
        <h2 id="note-title">{t('fax.note')}</h2>
        <label class="sr-only" for="note">{t('fax.note')}</label>
        <textarea id="note" rows="4" maxlength="4000" bind:value={note} oninput={() => (noteSaved = false)}></textarea>
        {#if fax.note_updated_by}
          <p class="muted small">{t('fax.note_by', { by: fax.note_updated_by, at: fmt(fax.note_updated_at) })}</p>
        {/if}
        {#if conflict}
          <div class="conflict" role="alert">
            <p>{t('fax.note_conflict', { by: conflict.by ?? '?' })}</p>
            <blockquote>{conflict.note ?? ''}</blockquote>
            <button type="button" onclick={takeTheirs}>{t('fax.note_take_theirs')}</button>
            <button type="button" onclick={keepMine}>{t('fax.note_keep_mine')}</button>
          </div>
        {/if}
        <button type="button" class="primary" onclick={() => saveNote()} disabled={noteSaving || !noteDirty}>{t('fax.note_save')}</button>
        {#if noteSaved}<span class="ok" role="status">{t('common.saved')}</span>{/if}
      </section>

      <section aria-labelledby="info-title" class="card">
        <h2 id="info-title">{t('fax.details')}</h2>
        <dl class="details">
          <dt>{t('fax.ani')}</dt><dd>{fax.ani ?? t('fax.unknown_number')}</dd>
          <dt>{t('fax.dnis')}</dt><dd>{fax.dnis ?? ''}</dd>
          <dt>{t('fax.remote_ident')}</dt><dd>{fax.remote_ident ?? ''}</dd>
          <dt>{t('fax.trunk')}</dt><dd>{fax.trunk ?? ''}</dd>
          <dt>{t('fax.started_at')}</dt><dd>{fmt(fax.started_at)}</dd>
          <dt>{t('fax.ended_at')}</dt><dd>{fmt(fax.ended_at)}</dd>
          <dt>{t('fax.duration')}</dt><dd>{formatDuration(fax.duration_sec)}</dd>
          <dt>{t('fax.pages')}</dt><dd>{fax.pages}</dd>
          <dt>{t('fax.hangup_label')}</dt><dd>{hangup(fax)}{fax.hangup_direction ? ` (${t(`fax.direction.${fax.hangup_direction}`)})` : ''}</dd>
          {#if fax.sip_status !== null}<dt>{t('fax.sip_status')}</dt><dd>{fax.sip_status}</dd>{/if}
          {#if fax.t30_code !== null}<dt>{t('fax.t30_code')}</dt><dd>{fax.t30_code}</dd>{/if}
          <dt>{t('fax.resolution')}</dt><dd>{fax.resolution ? t(`fax.resolution_value.${fax.resolution}`) : ''}</dd>
          <dt>{t('fax.bit_rate')}</dt><dd>{fax.bit_rate ?? ''}</dd>
          <dt>{t('fax.compression')}</dt><dd>{fax.compression ?? ''}</dd>
          <dt>{t('fax.ecm')}</dt><dd>{fax.ecm === null ? '' : t(fax.ecm ? 'common.yes' : 'common.no')}</dd>
          <dt>{t('fax.files')}</dt><dd>{[fax.has_pdf ? `PDF ${formatBytes(fax.pdf_size)}` : '', fax.has_tiff ? `TIFF ${formatBytes(fax.tiff_size)}` : ''].filter(Boolean).join(', ')}</dd>
          {#if fax.read_changed_by}<dt>{t('fax.read_by')}</dt><dd>{fax.read_changed_by} · {fmt(fax.read_changed_at)}</dd>{/if}
          <dt>{t('fax.call_id')}</dt><dd class="mono">{fax.call_id}</dd>
        </dl>
      </section>
    </div>

    {#if fax.deliveries !== undefined}
      <section aria-labelledby="deliveries-title" class="card">
        <h2 id="deliveries-title">{t('fax.deliveries')}</h2>
        {#if fax.deliveries.length === 0}
          <p class="muted">{t('fax.no_deliveries')}</p>
        {:else}
          <div class="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">{t('delivery.kind')}</th>
                  <th scope="col">{t('delivery.target')}</th>
                  <th scope="col">{t('delivery.status_label')}</th>
                  <th scope="col" class="num">{t('delivery.attempt')}</th>
                  <th scope="col">{t('delivery.next_try')}</th>
                  <th scope="col">{t('delivery.last_error')}</th>
                </tr>
              </thead>
              <tbody>
                {#each fax.deliveries as d (d.id)}
                  <tr>
                    <td>{t(`delivery.kind_value.${d.kind}`)}</td>
                    <td class="mono">{d.target.replace(/^[a-z]+:/, '')}</td>
                    <td><span class="badge delivery-{d.status}">{t(`delivery.status.${d.status}`)}</span></td>
                    <td class="num">{d.attempt}</td>
                    <td>{fmt(d.next_try_at)}</td>
                    <td class="mono small">{d.last_error ?? ''}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {/if}
  {/if}
</section>
