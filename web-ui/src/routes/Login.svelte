<script lang="ts">
  import { errorText } from '../lib/errors';
  import { i18n, LOCALES, t, type Locale } from '../lib/i18n.svelte';
  import { login, storeLocale } from '../lib/session.svelte';

  let username = $state('');
  let password = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      await login(username.trim(), password);
    } catch (err) {
      error = errorText(err);
      password = '';
    } finally {
      busy = false;
    }
  }

  function setLocale(e: Event) {
    const l = (e.currentTarget as HTMLSelectElement).value as Locale;
    i18n.set(l);
    storeLocale(l);
  }
</script>

<main class="center">
  <form class="card narrow" onsubmit={submit} aria-labelledby="login-title">
    <h1 id="login-title">{t('app.title')}</h1>
    <p class="muted">{t('login.subtitle')}</p>
    <label for="username">{t('login.username')}</label>
    <input id="username" name="username" autocomplete="username" required bind:value={username} />
    <label for="password">{t('login.password')}</label>
    <input id="password" name="password" type="password" autocomplete="current-password" required bind:value={password} />
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <button type="submit" class="primary" disabled={busy}>{t(busy ? 'login.signing_in' : 'login.submit')}</button>
    <label class="inline" for="login-locale">{t('nav.language')}</label>
    <select id="login-locale" value={i18n.locale} onchange={setLocale}>
      {#each LOCALES as l (l)}
        <option value={l}>{t(`locale.${l}`)}</option>
      {/each}
    </select>
  </form>
</main>
