<script lang="ts">
  import { onMount } from 'svelte';
  import { get, query, type AuditEntry, type AuditPage } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { fmt, hasKey, t } from '../lib/i18n.svelte';

  let entries = $state<AuditEntry[]>([]);
  let next = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<unknown>(null);
  let open = $state<number | null>(null);

  async function load(before: number | null) {
    loading = true;
    error = null;
    try {
      const p = await get<AuditPage>(`/config/audit${query({ limit: 50, before })}`);
      entries = before === null ? p.entries : [...entries, ...p.entries];
      next = p.next;
    } catch (e) {
      error = e;
    } finally {
      loading = false;
    }
  }
  onMount(() => load(null));

  function action(a: string): string {
    return hasKey(`audit.action.${a}`) ? t(`audit.action.${a}`) : a;
  }

  function sectionLabel(s: string): string {
    if (s.startsWith('trunk:')) return t('config.trunk_title', { name: s.slice(6) });
    if (s === 'trunk_order') return t('config.trunk_order');
    return hasKey(`config.section.${s}`) ? t(`config.section.${s}`) : s;
  }

  function json(v: unknown): string {
    return v === null || v === undefined ? '' : JSON.stringify(v, null, 2);
  }
</script>

<section class="page" aria-labelledby="audit-title">
  <h1 id="audit-title">{t('audit.title')}</h1>
  {#if error}<p class="error" role="alert">{errorText(error)}</p>{/if}
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th scope="col">{t('audit.at')}</th>
          <th scope="col">{t('audit.by')}</th>
          <th scope="col">{t('audit.section')}</th>
          <th scope="col">{t('audit.action_label')}</th>
          <th scope="col"><span class="sr-only">{t('audit.details')}</span></th>
        </tr>
      </thead>
      <tbody>
        {#each entries as e (e.id)}
          <tr>
            <td>{fmt(e.at)}</td>
            <td>{e.by}</td>
            <td>{sectionLabel(e.section)}</td>
            <td>{action(e.action)}</td>
            <td>
              <button type="button" class="small" aria-expanded={open === e.id} onclick={() => (open = open === e.id ? null : e.id)}>{t('audit.details')}</button>
            </td>
          </tr>
          {#if open === e.id}
            <tr class="details-row">
              <td colspan="5">
                <div class="diff">
                  <div><h3>{t('audit.before')}</h3><pre>{json(e.before)}</pre></div>
                  <div><h3>{t('audit.after')}</h3><pre>{json(e.after)}</pre></div>
                </div>
              </td>
            </tr>
          {/if}
        {:else}
          {#if !loading}<tr><td colspan="5" class="empty">{t('audit.empty')}</td></tr>{/if}
        {/each}
      </tbody>
    </table>
  </div>
  {#if loading}<p class="muted" role="status">{t('app.loading')}</p>{/if}
  {#if next !== null && !loading}<button type="button" onclick={() => load(next)}>{t('faxes.more')}</button>{/if}
</section>
