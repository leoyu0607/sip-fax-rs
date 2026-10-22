<script lang="ts">
  import { errorText } from '../lib/errors';
  import { t } from '../lib/i18n.svelte';
  import { changePassword, logout, session } from '../lib/session.svelte';

  let { forced = false }: { forced?: boolean } = $props();

  let current = $state('');
  let next = $state('');
  let again = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let done = $state(false);

  // the server checks the rules (spec 2.3); these only explain them early
  const problem = $derived.by(() => {
    if (next === '') return null;
    if ([...next].length < 10) return t('error.password_too_short');
    if (next === session.user?.username) return t('error.password_same_as_username');
    if (again !== '' && again !== next) return t('password.mismatch');
    return null;
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (problem !== null || next !== again) return;
    busy = true;
    error = null;
    try {
      await changePassword(current, next);
      current = next = again = '';
      done = true;
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:element this={forced ? 'main' : 'div'} class={forced ? 'center' : undefined}>
  <form class="card narrow" onsubmit={submit} aria-labelledby="pw-title">
    <h1 id="pw-title">{t('password.title')}</h1>
    {#if forced}<p>{t('password.forced')}</p>{/if}
    <label for="pw-current">{t('password.current')}</label>
    <input id="pw-current" type="password" autocomplete="current-password" required bind:value={current} />
    <label for="pw-new">{t('password.new')}</label>
    <input id="pw-new" type="password" autocomplete="new-password" required minlength="10" aria-describedby="pw-rules" bind:value={next} />
    <p id="pw-rules" class="muted small">{t('password.rules')}</p>
    <label for="pw-again">{t('password.again')}</label>
    <input id="pw-again" type="password" autocomplete="new-password" required bind:value={again} />
    {#if problem}<p class="error" role="alert">{problem}</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if done && !forced}<p class="ok" role="status">{t('password.changed')}</p>{/if}
    <button type="submit" class="primary" disabled={busy || problem !== null || next !== again || next === ''}>{t('password.submit')}</button>
    {#if forced}
      <button type="button" class="link" onclick={() => logout()}>{t('nav.logout')}</button>
    {/if}
  </form>
</svelte:element>
