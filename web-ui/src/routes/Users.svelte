<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { del, get, post, put, type Role, type User } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { fmt, LOCALES, t } from '../lib/i18n.svelte';
  import { session } from '../lib/session.svelte';

  let users = $state<User[]>([]);
  let loadError = $state<unknown>(null);
  let actionError = $state<string | null>(null);
  let message = $state<string | null>(null);

  // the form of a new account, opened by its button
  let creating = $state(false);
  let nu = $state({ username: '', display_name: '', role: 'agent' as Role, password: '', locale: '', must_change_password: true });
  // the account being edited
  let edit = $state<{ id: number; display_name: string; role: Role; locale: string; disabled: boolean; password: string; must_change_password: boolean } | null>(null);

  async function load() {
    try {
      users = (await get<{ users: User[] }>('/users')).users;
      loadError = null;
    } catch (e) {
      loadError = e;
    }
  }
  onMount(load);

  async function create(e: SubmitEvent) {
    e.preventDefault();
    actionError = message = null;
    try {
      await post('/users', {
        username: nu.username.trim(),
        role: nu.role,
        password: nu.password,
        display_name: nu.display_name.trim() || null,
        locale: nu.locale || null,
        must_change_password: nu.must_change_password,
      });
      message = t('users.created', { name: nu.username.trim() });
      nu = { username: '', display_name: '', role: 'agent', password: '', locale: '', must_change_password: true };
      creating = false;
      await load();
    } catch (err) {
      actionError = errorText(err);
    }
  }

  // one form at a time, above the list, with the focus in its first field
  async function openForm(focus: string) {
    actionError = message = null;
    await tick();
    document.getElementById(focus)?.focus();
  }

  function startCreate() {
    edit = null;
    creating = true;
    void openForm('n-user');
  }

  function startEdit(u: User) {
    creating = false;
    edit = { id: u.id, display_name: u.display_name ?? '', role: u.role, locale: u.locale ?? '', disabled: u.disabled, password: '', must_change_password: true };
    void openForm('e-dn');
  }

  async function saveEdit(e: SubmitEvent) {
    e.preventDefault();
    if (!edit) return;
    actionError = message = null;
    const body: Record<string, unknown> = {
      display_name: edit.display_name.trim() || null,
      role: edit.role,
      locale: edit.locale || null,
      disabled: edit.disabled,
    };
    if (edit.password !== '') {
      body.password = edit.password;
      body.must_change_password = edit.must_change_password;
    }
    try {
      await put(`/users/${edit.id}`, body);
      message = t('common.saved');
      edit = null;
      await load();
    } catch (err) {
      actionError = errorText(err);
    }
  }

  async function remove(u: User) {
    if (!confirm(t('users.delete_confirm', { name: u.username }))) return;
    actionError = message = null;
    try {
      await del(`/users/${u.id}`);
      await load();
    } catch (err) {
      actionError = errorText(err);
    }
  }
</script>

<section class="page" aria-labelledby="users-title">
  <div class="page-head">
    <h1 id="users-title">{t('users.title')}</h1>
    {#if !creating}<button type="button" class="primary" onclick={startCreate}>+ {t('users.new')}</button>{/if}
  </div>
  {#if loadError}<p class="error" role="alert">{errorText(loadError)}</p>{/if}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  {#if message}<p class="ok" role="status">{message}</p>{/if}

  {#if creating}
  <form class="card user-form" onsubmit={create} aria-labelledby="new-title">
    <h2 id="new-title">{t('users.new')}</h2>
    <div class="field"><label for="n-user">{t('users.username')}</label><input id="n-user" required autocomplete="off" bind:value={nu.username} /></div>
    <div class="field"><label for="n-dn">{t('users.display_name')}</label><input id="n-dn" bind:value={nu.display_name} /></div>
    <div class="field">
      <label for="n-role">{t('users.role')}</label>
      <select id="n-role" bind:value={nu.role}><option value="agent">{t('role.agent')}</option><option value="admin">{t('role.admin')}</option></select>
    </div>
    <div class="field">
      <label for="n-locale">{t('users.locale')}</label>
      <select id="n-locale" bind:value={nu.locale}>
        <option value="">{t('users.locale_browser')}</option>
        {#each LOCALES as l (l)}<option value={l}>{t(`locale.${l}`)}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="n-pw">{t('users.initial_password')}</label>
      <input id="n-pw" type="password" autocomplete="new-password" required minlength="10" bind:value={nu.password} aria-describedby="n-pw-help" />
      <p id="n-pw-help" class="muted small">{t('password.rules')}</p>
    </div>
    <label class="check"><input type="checkbox" bind:checked={nu.must_change_password} /> {t('users.must_change_next')}</label>
    <div class="actions">
      <button type="submit" class="primary">{t('users.create')}</button>
      <button type="button" onclick={() => (creating = false)}>{t('common.cancel')}</button>
    </div>
  </form>
  {/if}
  {#if edit}
    <form class="card user-form" onsubmit={saveEdit} aria-labelledby="edit-title">
      <h2 id="edit-title">{t('users.edit', { name: users.find((u) => u.id === edit?.id)?.username ?? '' })}</h2>
      <div class="field"><label for="e-dn">{t('users.display_name')}</label><input id="e-dn" bind:value={edit.display_name} /></div>
      <div class="field">
        <label for="e-role">{t('users.role')}</label>
        <select id="e-role" bind:value={edit.role}><option value="agent">{t('role.agent')}</option><option value="admin">{t('role.admin')}</option></select>
      </div>
      <div class="field">
        <label for="e-locale">{t('users.locale')}</label>
        <select id="e-locale" bind:value={edit.locale}>
          <option value="">{t('users.locale_browser')}</option>
          {#each LOCALES as l (l)}<option value={l}>{t(`locale.${l}`)}</option>{/each}
        </select>
      </div>
      <label class="check"><input type="checkbox" bind:checked={edit.disabled} /> {t('users.disabled')}</label>
      <div class="field">
        <label for="e-pw">{t('users.reset_password')}</label>
        <input id="e-pw" type="password" autocomplete="new-password" bind:value={edit.password} aria-describedby="e-pw-help" />
        <p id="e-pw-help" class="muted small">{t('users.reset_password_help')}</p>
      </div>
      {#if edit.password !== ''}
        <label class="check"><input type="checkbox" bind:checked={edit.must_change_password} /> {t('users.must_change_next')}</label>
      {/if}
      <div class="actions">
        <button type="submit" class="primary">{t('common.save')}</button>
        <button type="button" onclick={() => (edit = null)}>{t('common.cancel')}</button>
      </div>
    </form>
  {/if}

  <div class="table-wrap">
    <table class="users">
      <thead>
        <tr>
          <th scope="col">{t('users.username')}</th>
          <th scope="col">{t('users.display_name')}</th>
          <th scope="col">{t('users.role')}</th>
          <th scope="col">{t('users.state')}</th>
          <th scope="col">{t('users.last_login')}</th>
          <th scope="col"><span class="sr-only">{t('common.actions')}</span></th>
        </tr>
      </thead>
      <tbody>
        {#each users as u (u.id)}
          <tr>
            <td>{u.username}{#if u.id === session.user?.id} <span class="muted small">({t('users.you')})</span>{/if}</td>
            <td>{u.display_name ?? ''}</td>
            <td>{t(`role.${u.role}`)}</td>
            <td>
              {#if u.disabled}<span class="badge status-failed"><span aria-hidden="true">✕</span> {t('users.disabled')}</span>
              {:else}<span class="badge status-received"><span aria-hidden="true">✓</span> {t('users.enabled')}</span>{/if}
              {#if u.must_change_password}<span class="badge">{t('users.must_change')}</span>{/if}
            </td>
            <td>{fmt(u.last_login_at)}</td>
            <td class="row-actions">
              <div class="toolbar">
                <button type="button" class="small" onclick={() => startEdit(u)} aria-label={t('users.edit', { name: u.username })}>{t('common.edit')}</button>
                <button type="button" class="small danger" onclick={() => remove(u)} aria-label={t('users.delete_of', { name: u.username })}>{t('common.delete')}</button>
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

</section>
