<script lang="ts">
  import { put, type User } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { i18n, LOCALES, pickLocale, t, type Locale } from '../lib/i18n.svelte';
  import { session, storedLocale, updateUser } from '../lib/session.svelte';
  import ChangePassword from './ChangePassword.svelte';

  let locale = $state(session.user?.locale ?? '');
  let saved = $state(false);
  let error = $state<string | null>(null);

  async function saveLocale(e: SubmitEvent) {
    e.preventDefault();
    saved = false;
    error = null;
    try {
      const r = await put<{ user: User }>('/auth/me', { locale: locale === '' ? null : locale });
      updateUser(r.user);
      i18n.set(pickLocale(r.user.locale ?? storedLocale(), navigator.languages) as Locale);
      saved = true;
    } catch (err) {
      error = errorText(err);
    }
  }
</script>

<section class="page" aria-labelledby="account-title">
  <h1 id="account-title">{t('account.title')}</h1>
  <p>{session.user?.username} · {t(`role.${session.user?.role ?? 'agent'}`)}</p>
  <form class="card narrow" onsubmit={saveLocale}>
    <h2>{t('account.language')}</h2>
    <label for="acc-locale">{t('users.locale')}</label>
    <select id="acc-locale" bind:value={locale}>
      <option value="">{t('users.locale_browser')}</option>
      {#each LOCALES as l (l)}<option value={l}>{t(`locale.${l}`)}</option>{/each}
    </select>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if saved}<p class="ok" role="status">{t('common.saved')}</p>{/if}
    <button type="submit" class="primary">{t('common.save')}</button>
  </form>
  <ChangePassword />
</section>
