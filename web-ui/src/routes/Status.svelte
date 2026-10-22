<script lang="ts">
  import { onMount } from 'svelte';
  import { get, type Status } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { fmt, formatDuration, t } from '../lib/i18n.svelte';
  import { onEvent } from '../lib/live.svelte';
  import { session } from '../lib/session.svelte';

  let status = $state<Status | null>(null);
  let error = $state<unknown>(null);

  async function load() {
    try {
      status = await get<Status>('/status');
      error = null;
    } catch (e) {
      error = e;
    }
  }

  onMount(() => {
    load();
    const off = onEvent((e) => {
      if (e.type !== 'status.changed' || !status) return;
      const d = e.data as { state?: string; active_calls?: number; trunks?: Status['service']['trunks']; pending_restart?: string[] };
      status.service = {
        ...status.service,
        state: d.state ?? status.service.state,
        active_calls: d.active_calls ?? status.service.active_calls,
        trunks: d.trunks ?? status.service.trunks,
      };
      if (d.pending_restart) status.config.pending_restart = d.pending_restart;
    });
    // the uptime and the database state are not pushed
    const timer = setInterval(load, 30_000);
    return () => {
      off();
      clearInterval(timer);
    };
  });

  function registration(r: string): string {
    const [head, ...rest] = r.split(':');
    const key = `status.registration.${head?.trim()}`;
    const known = ['none', 'registering', 'registered', 'failed', 'unregistered', 'unknown'].includes(head?.trim() ?? '');
    return known ? `${t(key)}${rest.length ? `: ${rest.join(':').trim()}` : ''}` : r;
  }
</script>

<section class="page" aria-labelledby="status-title">
  <h1 id="status-title">{t('status.title')}</h1>
  {#if error}<p class="error" role="alert">{errorText(error)}</p>{/if}
  {#if status}
    <div class="columns">
      <section class="card" aria-labelledby="svc-title">
        <h2 id="svc-title">{t('status.service')}</h2>
        <dl class="details">
          <dt>{t('status.state')}</dt>
          <dd><span class="badge status-{status.service.state === 'running' ? 'received' : 'partial'}"><span aria-hidden="true">{status.service.state === 'running' ? '✓' : '◐'}</span> {t(`status.state_value.${status.service.state}`)}</span></dd>
          <dt>{t('status.version')}</dt><dd>{status.service.version}</dd>
          <dt>{t('status.started_at')}</dt><dd>{fmt(status.service.started_at)}</dd>
          <dt>{t('status.uptime')}</dt><dd>{formatDuration(status.service.uptime_secs)}</dd>
          <dt>{t('status.listen')}</dt><dd class="mono">{status.service.listen.join(', ')}</dd>
          <dt>{t('status.active_calls')}</dt><dd>{status.service.active_calls}</dd>
        </dl>
      </section>
      <section class="card" aria-labelledby="db-title">
        <h2 id="db-title">{t('status.database')}</h2>
        <p>
          {#if status.database.state === 'ok'}
            <span class="badge status-received"><span aria-hidden="true">✓</span> {t('status.db_ok')}</span>
            <span class="muted small">{t('status.schema', { n: status.database.schema ?? '' })}</span>
          {:else}
            <span class="badge status-failed"><span aria-hidden="true">✕</span> {t('status.db_unavailable')}</span>
          {/if}
        </p>
        <h2>{t('status.config')}</h2>
        <p>{t(`status.config_source.${status.config.source}`)}</p>
        {#if status.config.pending_restart.length > 0}
          <p class="restart"><span aria-hidden="true">⟲</span> {t('config.pending_restart', { fields: status.config.pending_restart.join(', ') })}</p>
        {/if}
        {#if session.isAdmin && (status.config.undecryptable ?? []).length > 0}
          <p class="error">{t('config.undecryptable', { fields: (status.config.undecryptable ?? []).join(', ') })}</p>
        {/if}
      </section>
    </div>
    <section class="card" aria-labelledby="trunks-title">
      <h2 id="trunks-title">{t('status.trunks')}</h2>
      {#if status.service.trunks.length === 0}
        <p class="muted">{t('config.no_trunks')}</p>
      {:else}
        <table>
          <thead><tr><th scope="col">{t('status.trunk')}</th><th scope="col">{t('status.registration_label')}</th></tr></thead>
          <tbody>
            {#each status.service.trunks as tr (tr.name)}
              <tr><td>{tr.name}</td><td>{registration(tr.registration)}</td></tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}
</section>
