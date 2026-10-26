<script lang="ts">
  // API keys for other systems (M6a, ADR-070): list, a new key shown once, revoke.
  import { onMount, tick } from 'svelte';
  import { get, post, type ApiKey } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { fmt, t } from '../lib/i18n.svelte';

  let keys = $state<ApiKey[]>([]);
  let loadError = $state<unknown>(null);
  let actionError = $state<string | null>(null);

  let creating = $state(false);
  let nk = $state({ name: '', write: false, expires: '' });
  // the key just created: shown until the administrator closes it, never again
  let created = $state<{ name: string; key: string } | null>(null);
  let copied = $state(false);

  async function load() {
    try {
      keys = (await get<{ keys: ApiKey[] }>('/api-keys')).keys;
      loadError = null;
    } catch (e) {
      loadError = e;
    }
  }
  onMount(load);

  async function startCreate() {
    actionError = null;
    created = null;
    creating = true;
    await tick();
    document.getElementById('k-name')?.focus();
  }

  /** The last valid day in the viewer's time zone: valid until the end of that day. */
  function expiryOf(day: string): string | null {
    if (day === '') return null;
    const [y, m, d] = day.split('-').map(Number);
    return new Date(y ?? 0, (m ?? 1) - 1, (d ?? 1) + 1).toISOString();
  }

  async function create(e: SubmitEvent) {
    e.preventDefault();
    actionError = null;
    try {
      const r = await post<{ key: string; api_key: ApiKey }>('/api-keys', {
        name: nk.name.trim(),
        write: nk.write,
        expires_at: expiryOf(nk.expires),
      });
      created = { name: r.api_key.name, key: r.key };
      copied = false;
      nk = { name: '', write: false, expires: '' };
      creating = false;
      await load();
    } catch (err) {
      actionError = errorText(err);
    }
  }

  async function copy() {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created.key);
      copied = true;
    } catch {
      // no clipboard (plain HTTP, permissions): the key stays selectable
      document.getElementById('k-created')?.focus();
    }
  }

  async function revoke(k: ApiKey) {
    if (!confirm(t('apikeys.revoke_confirm', { name: k.name }))) return;
    actionError = null;
    try {
      await post(`/api-keys/${k.id}/revoke`, {});
      await load();
    } catch (err) {
      actionError = errorText(err);
    }
  }

  function keyState(k: ApiKey): 'active' | 'revoked' | 'expired' {
    if (k.revoked_at) return 'revoked';
    return k.active ? 'active' : 'expired';
  }
</script>

<section class="page" aria-labelledby="keys-title">
  <div class="page-head">
    <h1 id="keys-title">{t('apikeys.title')}</h1>
    {#if !creating}<button type="button" class="primary" onclick={startCreate}>+ {t('apikeys.new')}</button>{/if}
  </div>
  <p class="muted">{t('apikeys.intro')}</p>
  {#if loadError}<p class="error" role="alert">{errorText(loadError)}</p>{/if}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

  {#if created}
    <div class="card key-created" role="status" aria-labelledby="created-title">
      <h2 id="created-title">{t('apikeys.created', { name: created.name })}</h2>
      <p>{t('apikeys.created_once')}</p>
      <div class="key-row">
        <input id="k-created" class="key" readonly value={created.key} aria-label={t('apikeys.key')} onfocus={(e) => (e.currentTarget as HTMLInputElement).select()} />
        <button type="button" onclick={copy}>{copied ? t('apikeys.copied') : t('apikeys.copy')}</button>
      </div>
      <div class="actions"><button type="button" class="primary" onclick={() => (created = null)}>{t('apikeys.done')}</button></div>
    </div>
  {/if}

  {#if creating}
    <form class="card user-form" onsubmit={create} aria-labelledby="new-key-title">
      <h2 id="new-key-title">{t('apikeys.new')}</h2>
      <div class="field">
        <label for="k-name">{t('apikeys.name')}</label>
        <input id="k-name" required autocomplete="off" maxlength="48" bind:value={nk.name} aria-describedby="k-name-help" />
        <p id="k-name-help" class="muted small">{t('apikeys.name_help')}</p>
      </div>
      <label class="check"><input type="checkbox" bind:checked={nk.write} /> {t('apikeys.write')}</label>
      <p class="muted small">{t('apikeys.write_help')}</p>
      <div class="field">
        <label for="k-exp">{t('apikeys.expires')}</label>
        <input id="k-exp" type="date" bind:value={nk.expires} aria-describedby="k-exp-help" />
        <p id="k-exp-help" class="muted small">{t('apikeys.expires_help')}</p>
      </div>
      <div class="actions">
        <button type="submit" class="primary">{t('apikeys.create')}</button>
        <button type="button" onclick={() => (creating = false)}>{t('common.cancel')}</button>
      </div>
    </form>
  {/if}

  <div class="table-wrap">
    <table class="users">
      <thead>
        <tr>
          <th scope="col">{t('apikeys.name')}</th>
          <th scope="col">{t('apikeys.prefix')}</th>
          <th scope="col">{t('apikeys.scopes')}</th>
          <th scope="col">{t('apikeys.state')}</th>
          <th scope="col">{t('apikeys.expires')}</th>
          <th scope="col">{t('apikeys.last_used')}</th>
          <th scope="col">{t('apikeys.created_by')}</th>
          <th scope="col"><span class="sr-only">{t('common.actions')}</span></th>
        </tr>
      </thead>
      <tbody>
        {#each keys as k (k.id)}
          <tr>
            <td>{k.name}</td>
            <td><code>{k.prefix}…</code></td>
            <td>{k.scopes.includes('fax:write') ? t('apikeys.scope_write') : t('apikeys.scope_read')}</td>
            <td>
              {#if keyState(k) === 'active'}<span class="badge status-received"><span aria-hidden="true">✓</span> {t('apikeys.active')}</span>
              {:else if keyState(k) === 'revoked'}<span class="badge status-failed"><span aria-hidden="true">✕</span> {t('apikeys.revoked')}</span>
              {:else}<span class="badge">{t('apikeys.expired')}</span>{/if}
            </td>
            <td>{k.expires_at ? fmt(k.expires_at) : t('apikeys.never')}</td>
            <td>{k.last_used_at ? `${fmt(k.last_used_at)} (${k.last_used_ip ?? ''})` : '—'}</td>
            <td>{k.created_by}<br /><span class="muted small">{fmt(k.created_at)}</span></td>
            <td class="row-actions">
              {#if !k.revoked_at}
                <button type="button" class="small danger" onclick={() => revoke(k)} aria-label={t('apikeys.revoke_of', { name: k.name })}>{t('apikeys.revoke')}</button>
              {/if}
            </td>
          </tr>
        {:else}
          <tr><td colspan="8" class="muted">{t('apikeys.none')}</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
</section>

<style>
  .key-row {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .key {
    flex: 1 1 24rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }
</style>
