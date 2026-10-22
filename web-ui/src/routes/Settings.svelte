<script lang="ts">
  import { onMount } from 'svelte';
  import { del, get, put, type ConfigList, type Section, type UpdateOutcome } from '../lib/api';
  import { errorText } from '../lib/errors';
  import { t } from '../lib/i18n.svelte';
  import { href } from '../lib/route';
  import { SCHEMAS, SIMPLE_SECTIONS, newTrunk, type SectionKind } from '../lib/schema';
  import SectionEditor from '../components/SectionEditor.svelte';
  import RoutesEditor from '../components/RoutesEditor.svelte';

  let { section }: { section: string | null } = $props();

  let config = $state<ConfigList | null>(null);
  let loadError = $state<unknown>(null);
  let actionError = $state<string | null>(null);
  let newName = $state('');
  let orderSaved = $state(false);
  let generation = $state(0);

  async function load() {
    try {
      config = await get<ConfigList>('/config');
      loadError = null;
      generation += 1;
    } catch (e) {
      loadError = e;
    }
  }

  onMount(() => {
    load();
  });

  const NAV = ['sip', 'media', 'call', 'capture', 'trunks', 'routes', 'delivery', 'retention'] as const;
  const current = $derived(section ?? 'sip');
  const trunks = $derived((config?.sections ?? []).filter((s) => s.name.startsWith('trunk:')));
  const restartFields = $derived(config?.restart_fields ?? []);
  const pending = $derived(config?.status.pending_restart ?? []);

  // a new trunk is edited before it exists (version 0)
  let draftTrunk = $state<Section | null>(null);
  const sectionData = $derived.by((): Section | null => {
    if (!config) return null;
    if (draftTrunk && current === draftTrunk.name) return draftTrunk;
    return config.sections.find((s) => s.name === current) ?? null;
  });

  function kindOf(name: string): SectionKind | null {
    if (name.startsWith('trunk:')) return 'trunk';
    return (SIMPLE_SECTIONS as readonly string[]).includes(name) ? (name as SectionKind) : null;
  }

  function saved(o: UpdateOutcome, body: unknown) {
    if (!config) return;
    const existing = config.sections.find((s) => s.name === o.section);
    if (existing) {
      existing.version = o.version;
      existing.body = body;
    } else {
      config.sections.push({ name: o.section, version: o.version, body });
    }
    config.status.pending_restart = o.pending_restart;
    // a new trunk is now one of the sections (the server put it last in the order, as here);
    // the form stays as it is, with its "saved" message
    if (draftTrunk?.name === o.section) draftTrunk = null;
  }

  function addTrunk(e: SubmitEvent) {
    e.preventDefault();
    const n = newName.trim();
    if (n === '') return;
    draftTrunk = { name: `trunk:${n}`, version: 0, body: newTrunk(n) };
    newName = '';
    location.hash = href({ page: 'settings', section: `trunk:${n}` });
  }

  async function removeTrunk(s: Section) {
    if (!confirm(t('config.delete_trunk_confirm', { name: s.name.slice(6) }))) return;
    actionError = null;
    try {
      await del(`/config/${encodeURIComponent(s.name)}?version=${s.version}`);
      await load();
      location.hash = href({ page: 'settings', section: 'trunks' });
    } catch (e) {
      actionError = errorText(e);
    }
  }

  async function moveTrunk(i: number, by: number) {
    const names = trunks.map((s) => s.name.slice(6));
    const j = i + by;
    if (j < 0 || j >= names.length) return;
    [names[i], names[j]] = [names[j] as string, names[i] as string];
    actionError = null;
    orderSaved = false;
    try {
      await put('/config/trunks/order', { order: names });
      await load();
      orderSaved = true;
    } catch (e) {
      actionError = errorText(e);
    }
  }
</script>

<section class="page settings" aria-labelledby="settings-title">
  <h1 id="settings-title">{t('config.title')}</h1>

  {#if loadError}
    <p class="error" role="alert">{errorText(loadError)} <button type="button" onclick={load}>{t('common.retry')}</button></p>
  {:else if config === null}
    <p class="muted">{t('app.loading')}</p>
  {:else}
    {#if config.status.source === 'file_fallback'}
      <p class="notice" role="note">{t('config.file_fallback')}</p>
    {/if}
    {#if pending.length > 0}
      <p class="notice restart" role="note"><span aria-hidden="true">⟲</span> {t('config.pending_restart', { fields: pending.join(', ') })}</p>
    {/if}
    {#if (config.status.undecryptable ?? []).length > 0}
      <p class="notice error" role="note">{t('config.undecryptable', { fields: (config.status.undecryptable ?? []).join(', ') })}</p>
    {/if}

    <div class="settings-layout">
      <nav aria-label={t('config.sections')}>
        <ul class="side-nav">
          {#each NAV as n (n)}
            <li>
              <a href={href({ page: 'settings', section: n })} aria-current={current === n || (n === 'trunks' && current.startsWith('trunk:')) ? 'page' : undefined}>{t(`config.section.${n}`)}</a>
            </li>
          {/each}
        </ul>
      </nav>

      <div class="settings-body">
        {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
        {#if current === 'trunks'}
          <h2>{t('config.section.trunks')}</h2>
          <p class="muted">{t('config.trunks_help')}</p>
          {#if orderSaved}<p class="ok" role="status">{t('config.order_saved')}</p>{/if}
          <ol class="trunk-list">
            {#each trunks as s, i (s.name)}
              <li>
                <a href={href({ page: 'settings', section: s.name })}>{s.name.slice(6)}</a>
                <span class="muted small">{(s.body as Record<string, unknown>)?.peer ?? ''}</span>
                <span class="toolbar">
                  <button type="button" class="small" onclick={() => moveTrunk(i, -1)} disabled={i === 0} aria-label={t('config.move_up_of', { n: s.name.slice(6) })}>↑</button>
                  <button type="button" class="small" onclick={() => moveTrunk(i, 1)} disabled={i === trunks.length - 1} aria-label={t('config.move_down_of', { n: s.name.slice(6) })}>↓</button>
                  <button type="button" class="small danger" onclick={() => removeTrunk(s)}>{t('common.delete')}</button>
                </span>
              </li>
            {:else}
              <li class="muted">{t('config.no_trunks')}</li>
            {/each}
          </ol>
          <form class="inline-form" onsubmit={addTrunk}>
            <label for="new-trunk">{t('config.new_trunk')}</label>
            <input id="new-trunk" bind:value={newName} pattern="[A-Za-z0-9_.\-]+" required />
            <button type="submit">{t('common.add')}</button>
          </form>
        {:else if current === 'routes'}
          <h2>{t('config.section.routes')}</h2>
          {#if sectionData}
            {#key `${generation}:routes`}
              <SectionEditor name="routes" version={sectionData.version} body={sectionData.body} fields={[]} labels="config.field.routes" restartSection="" {restartFields} onsaved={saved} onreload={load}>
                {#snippet extra({ body, set, errors })}
                  <RoutesEditor {body} {set} {errors} />
                {/snippet}
              </SectionEditor>
            {/key}
          {/if}
        {:else if sectionData && kindOf(current)}
          {@const kind = kindOf(current) as SectionKind}
          <h2>
            {kind === 'trunk' ? t('config.trunk_title', { name: current.slice(6) }) : t(`config.section.${current}`)}
            {#if sectionData.version === 0}<span class="badge">{t('config.new')}</span>{/if}
          </h2>
          {#if kind === 'trunk'}<p><a href={href({ page: 'settings', section: 'trunks' })}>← {t('config.section.trunks')}</a></p>{/if}
          {#key `${generation}:${current}`}
            <SectionEditor
              name={current}
              version={sectionData.version}
              body={sectionData.body}
              fields={SCHEMAS[kind]}
              labels="config.field.{kind}"
              restartSection={kind === 'trunk' ? '' : current}
              {restartFields}
              readonly={kind === 'trunk' ? ['name'] : []}
              onsaved={saved}
              onreload={load}
            />
          {/key}
        {:else}
          <p>{t('error.not_found')}</p>
        {/if}
      </div>
    </div>
  {/if}
</section>
