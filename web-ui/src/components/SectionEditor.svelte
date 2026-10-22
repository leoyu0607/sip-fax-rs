<script lang="ts">
  import type { Snippet } from 'svelte';
  import { ApiError, put, type UpdateOutcome } from '../lib/api';
  import { errorText, fieldErrorText } from '../lib/errors';
  import { t } from '../lib/i18n.svelte';
  import { setPath, type Field } from '../lib/schema';
  import FieldInput from './FieldInput.svelte';

  let {
    name,
    version,
    body: initial,
    fields,
    labels,
    restartSection,
    restartFields,
    readonly = [],
    onsaved,
    onreload,
    extra,
  }: {
    /** `sip`, `trunk:cht`, ... */
    name: string;
    version: number;
    body: unknown;
    fields: readonly Field[];
    labels: string;
    /** Prefix of the restart names (`sip`); empty: nothing of it waits for a restart. */
    restartSection: string;
    restartFields: readonly string[];
    /** Keys that cannot be changed here (a trunk's name). */
    readonly?: readonly string[];
    onsaved: (o: UpdateOutcome, body: unknown) => void;
    onreload: () => void;
    /** More inputs of the same body (the routes editor). */
    extra?: Snippet<[{ body: unknown; set: (path: string, v: unknown) => void; errors: Record<string, unknown> }]>;
  } = $props();

  // svelte-ignore state_referenced_locally
  let body = $state<unknown>(structuredClone($state.snapshot(initial)));
  let errors = $state<Record<string, unknown>>({});
  let formError = $state<string | null>(null);
  let conflict = $state(false);
  let saving = $state(false);
  let outcome = $state<UpdateOutcome | null>(null);
  // svelte-ignore state_referenced_locally
  let saved = $state(JSON.stringify(initial));
  const dirty = $derived(JSON.stringify(body) !== saved);

  function set(path: string, v: unknown) {
    body = setPath(body, path, v);
    outcome = null;
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    saving = true;
    errors = {};
    formError = null;
    conflict = false;
    try {
      const o = await put<UpdateOutcome>(`/config/${encodeURIComponent(name)}`, { version, body });
      outcome = o;
      saved = JSON.stringify(body);
      onsaved(o, body);
    } catch (err) {
      if (err instanceof ApiError && err.code === 'invalid' && err.fields) {
        errors = err.fields;
        formError = t('config.invalid');
        // a problem of the whole section, or of a field this form does not show
        const whole = err.fields[''];
        if (whole !== undefined) formError = fieldErrorText(whole);
      } else if (err instanceof ApiError && err.code === 'conflict') {
        conflict = true;
      } else {
        formError = errorText(err);
      }
    } finally {
      saving = false;
    }
  }

  const visible = $derived(fields.filter((f) => !readonly.includes(f.key)));
  const unshown = $derived(
    Object.entries(errors).filter(([k]) => k !== '' && !fields.some((f) => k === f.key || k.startsWith(`${f.key}.`) || k.startsWith(`${f.key}[`)) && !k.startsWith('routes')),
  );
</script>

<form class="section-form" onsubmit={save} novalidate>
  {#each visible as f (f.key)}
    <FieldInput field={f} {body} path={f.key} {labels} section={restartSection} {errors} {restartFields} onchange={set} />
  {/each}
  {@render extra?.({ body, set, errors })}

  {#each unshown as [k, v] (k)}
    <p class="error small">{k}: {fieldErrorText(v)}</p>
  {/each}
  {#if formError}<p class="error" role="alert">{formError}</p>{/if}
  {#if conflict}
    <div class="conflict" role="alert">
      <p>{t('config.conflict')}</p>
      <button type="button" onclick={onreload}>{t('config.reload')}</button>
    </div>
  {/if}
  {#if outcome}
    <div class="ok" role="status">
      <p>{t('config.saved')}</p>
      {#if outcome.restart_required.length > 0}
        <p class="restart"><span aria-hidden="true">⟲</span> {t('config.restart_required', { fields: outcome.restart_required.join(', ') })}</p>
      {/if}
      {#if outcome.warnings.length > 0}
        <details class="warning">
          <summary>{t('config.warnings', { count: outcome.warnings.length })}</summary>
          <ul class="mono small">
            {#each outcome.warnings as w, i (i)}<li>{typeof w === 'string' ? w : JSON.stringify(w)}</li>{/each}
          </ul>
        </details>
      {/if}
    </div>
  {/if}
  <div class="actions sticky">
    <button type="submit" class="primary" disabled={saving || !dirty}>{t(saving ? 'common.saving' : 'common.save')}</button>
    {#if dirty}<span class="muted small">{t('config.unsaved')}</span>{/if}
  </div>
</form>
