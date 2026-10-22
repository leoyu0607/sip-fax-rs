<script lang="ts">
  import FieldInput from './FieldInput.svelte';
  import { fieldErrorText } from '../lib/errors';
  import { hasKey, t } from '../lib/i18n.svelte';
  import {
    getPath,
    joinPath,
    linesToList,
    linesToMap,
    mapToLines,
    needsRestart,
    secretIsSet,
    type Field,
  } from '../lib/schema';

  let {
    field,
    body,
    path,
    labels,
    section,
    errors,
    restartFields,
    onchange,
  }: {
    field: Field;
    /** The whole section body (values are read at `path`). */
    body: unknown;
    /** Path of this field from the section root (`t38.max_version`). */
    path: string;
    /** Translation prefix of the labels (`config.field.trunk`). */
    labels: string;
    /** Section name for the restart marks (`sip`); empty: none apply. */
    section: string;
    errors: Record<string, unknown>;
    restartFields: readonly string[];
    onchange: (path: string, value: unknown) => void;
  } = $props();

  const id = $derived(`f-${path.replace(/[^\w-]/g, '_')}`);
  // labels ignore list indices: routes[2].deliveries[0].url → delivery.<kind>.url (the caller's prefix)
  const labelKey = $derived(`${labels}.${field.key}`);
  const label = $derived(t(labelKey));
  const help = $derived(hasKey(`${labelKey}_help`) ? t(`${labelKey}_help`) : null);
  const value = $derived(getPath(body, path));
  const error = $derived(errors[path]);
  const restart = $derived(section !== '' && needsRestart(restartFields, section, path));
  const describedBy = $derived(
    [help ? `${id}-help` : null, error ? `${id}-err` : null, restart ? `${id}-restart` : null].filter(Boolean).join(' ') ||
      undefined,
  );

  function optionLabel(v: string): string {
    const k = `config.option.${field.key}.${v}`;
    return hasKey(k) ? t(k) : v;
  }

  function text(e: Event, nullable: boolean | undefined) {
    const v = (e.currentTarget as HTMLInputElement | HTMLTextAreaElement).value;
    onchange(path, nullable && v.trim() === '' ? null : v);
  }

  function number(e: Event, nullable: boolean | undefined) {
    const raw = (e.currentTarget as HTMLInputElement).value;
    if (raw.trim() === '') onchange(path, nullable ? null : raw);
    else {
      const n = Number(raw);
      onchange(path, Number.isFinite(n) ? n : raw);
    }
  }

  function toggleOption(opt: string, on: boolean, options: readonly string[]) {
    const cur = Array.isArray(value) ? (value as string[]) : [];
    const set = new Set(on ? [...cur, opt] : cur.filter((x) => x !== opt));
    onchange(path, options.filter((o) => set.has(o)));
  }

  // a secret: the stored one is kept unless a new one is typed or it is cleared
  let secretOriginal = $state<unknown>(undefined);
  let secretNew = $state('');
  $effect.pre(() => {
    if (field.kind !== 'secret') return;
    // what the server sent (`{"set", "ref"}`) is the stored one; a string or null is an edit
    if (typeof value === 'object' && value !== null) {
      secretOriginal = value;
      secretNew = '';
    }
    else if (secretOriginal === undefined) secretOriginal = typeof value === 'string' ? null : (value ?? null);
  });
  function secretInput(e: Event) {
    secretNew = (e.currentTarget as HTMLInputElement).value;
    onchange(path, secretNew === '' ? secretOriginal : secretNew);
  }
  function secretClear(e: Event) {
    const clear = (e.currentTarget as HTMLInputElement).checked;
    secretNew = '';
    onchange(path, clear ? null : secretOriginal);
  }
</script>

{#if field.kind === 'group'}
  <fieldset class="group">
    <legend>{t(`${labelKey}.label`)}</legend>
    {#each field.fields as f (f.key)}
      <FieldInput field={f} {body} path={joinPath(path, f.key)} labels={labelKey} {section} {errors} {restartFields} {onchange} />
    {/each}
  </fieldset>
{:else}
  <div class="field" class:has-error={error !== undefined}>
    {#if field.kind === 'bool'}
      <label class="check" for={id}>
        <input id={id} type="checkbox" checked={value === true} aria-describedby={describedBy} onchange={(e) => onchange(path, (e.currentTarget as HTMLInputElement).checked)} />
        {label}
      </label>
    {:else if field.kind === 'list' && field.options && !field.ordered}
      <fieldset class="options" aria-describedby={describedBy}>
        <legend>{label}</legend>
        {#each field.options as o (o)}
          <label class="check">
            <input type="checkbox" checked={Array.isArray(value) && value.includes(o)} onchange={(e) => toggleOption(o, (e.currentTarget as HTMLInputElement).checked, field.options ?? [])} />
            {optionLabel(o)}
          </label>
        {/each}
      </fieldset>
    {:else}
      <label for={id}>{label}</label>
      {#if field.kind === 'text' && field.multiline}
        <textarea id={id} rows="6" value={typeof value === 'string' ? value : ''} aria-describedby={describedBy} aria-invalid={error !== undefined} oninput={(e) => text(e, field.nullable)}></textarea>
      {:else if field.kind === 'text'}
        <input id={id} value={typeof value === 'string' ? value : ''} placeholder={field.placeholder ?? (field.nullable ? t('config.default_value') : undefined)} aria-describedby={describedBy} aria-invalid={error !== undefined} oninput={(e) => text(e, field.nullable)} />
      {:else if field.kind === 'number'}
        <input id={id} type="number" inputmode="decimal" step={field.step ?? 1} min={field.min} max={field.max} value={value === null || value === undefined ? '' : String(value)} placeholder={field.nullable ? t('config.default_value') : undefined} aria-describedby={describedBy} aria-invalid={error !== undefined} oninput={(e) => number(e, field.nullable)} />
      {:else if field.kind === 'select'}
        <select id={id} value={typeof value === 'string' ? value : ''} aria-describedby={describedBy} aria-invalid={error !== undefined} onchange={(e) => { const v = (e.currentTarget as HTMLSelectElement).value; onchange(path, v === '' && field.nullable ? null : v); }}>
          {#if field.nullable || typeof value !== 'string'}<option value="">{t('config.default_value')}</option>{/if}
          {#each field.options as o (o)}<option value={o}>{optionLabel(o)}</option>{/each}
        </select>
      {:else if field.kind === 'list'}
        <textarea id={id} rows="3" value={Array.isArray(value) ? value.join('\n') : ''} placeholder={field.options ? field.options.join('\n') : field.nullable ? t('config.default_value') : undefined} aria-describedby={describedBy} aria-invalid={error !== undefined} oninput={(e) => { const l = linesToList((e.currentTarget as HTMLTextAreaElement).value); onchange(path, l.length === 0 && field.nullable ? null : l); }}></textarea>
        <p class="muted small">{t('config.one_per_line')}{field.options ? ` (${field.options.join(', ')})` : ''}</p>
      {:else if field.kind === 'map'}
        <textarea id={id} rows="3" value={mapToLines(value)} placeholder="X-Header: value" aria-describedby={describedBy} aria-invalid={error !== undefined} oninput={(e) => onchange(path, linesToMap((e.currentTarget as HTMLTextAreaElement).value))}></textarea>
      {:else if field.kind === 'secret'}
        <p class="muted small">
          <span aria-hidden="true">{secretIsSet(secretOriginal) ? '🔒' : '○'}</span>
          {t(secretIsSet(secretOriginal) ? 'config.secret_set' : 'config.secret_unset')}
        </p>
        <input id={id} type="password" autocomplete="new-password" value={secretNew} placeholder={t('config.secret_new')} aria-describedby={describedBy} aria-invalid={error !== undefined} oninput={secretInput} disabled={value === null && secretIsSet(secretOriginal)} />
        {#if secretIsSet(secretOriginal)}
          <label class="check"><input type="checkbox" checked={value === null} onchange={secretClear} /> {t('config.secret_clear')}</label>
        {/if}
      {/if}
    {/if}
    {#if help}<p id="{id}-help" class="muted small">{help}</p>{/if}
    {#if restart}<p id="{id}-restart" class="restart small"><span aria-hidden="true">⟲</span> {t('config.restart_field')}</p>{/if}
    {#if error !== undefined}<p id="{id}-err" class="error small">{fieldErrorText(error)}</p>{/if}
  </div>
{/if}
