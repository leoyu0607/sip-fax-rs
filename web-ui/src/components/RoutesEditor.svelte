<script lang="ts">
  import { t } from '../lib/i18n.svelte';
  import {
    DELIVERY_FIELDS,
    DELIVERY_KINDS,
    ROUTE_FIELDS,
    joinPath,
    newDelivery,
    newRoute,
    type DeliveryKind,
  } from '../lib/schema';
  import FieldInput from './FieldInput.svelte';

  let {
    body,
    set,
    errors,
  }: {
    body: unknown;
    set: (path: string, v: unknown) => void;
    errors: Record<string, unknown>;
  } = $props();

  type Obj = Record<string, unknown>;
  const routes = $derived(((body as Obj | null)?.routes as Obj[] | undefined) ?? []);

  function move<T>(list: T[], i: number, by: number): T[] {
    const j = i + by;
    if (j < 0 || j >= list.length) return list;
    const out = [...list];
    [out[i], out[j]] = [out[j] as T, out[i] as T];
    return out;
  }

  function deliveries(r: Obj): Obj[] {
    return (r.deliveries as Obj[] | undefined) ?? [];
  }

  function setKind(i: number, j: number, kind: DeliveryKind) {
    set(`routes[${i}].deliveries[${j}]`, newDelivery(kind));
  }
</script>

<div class="routes">
  <p class="muted">{t('config.routes_help')}</p>
  {#if errors['routes'] !== undefined}<p class="error">{t('config.invalid')}</p>{/if}
  <ol class="route-list">
    {#each routes as r, i (i)}
      <li class="card">
        <div class="card-head">
          <h3>{t('config.route_n', { n: i + 1 })}{r.name ? ` · ${r.name}` : ''}</h3>
          <div class="toolbar">
            <button type="button" class="small" onclick={() => set('routes', move(routes, i, -1))} disabled={i === 0} aria-label={t('config.move_up_of', { n: i + 1 })}>↑</button>
            <button type="button" class="small" onclick={() => set('routes', move(routes, i, 1))} disabled={i === routes.length - 1} aria-label={t('config.move_down_of', { n: i + 1 })}>↓</button>
            <button type="button" class="small danger" onclick={() => set('routes', routes.filter((_, k) => k !== i))}>{t('config.remove_route')}</button>
          </div>
        </div>
        {#each ROUTE_FIELDS as f (f.key)}
          <FieldInput field={f} {body} path={joinPath(`routes[${i}]`, f.key)} labels="config.field.route" section="" {errors} restartFields={[]} onchange={set} />
        {/each}
        <h4>{t('config.deliveries')}</h4>
        {#if deliveries(r).length === 0}<p class="muted small">{t('config.no_deliveries')}</p>{/if}
        <ol class="delivery-list">
          {#each deliveries(r) as d, j (j)}
            {@const kind = (d.kind as DeliveryKind) ?? 'folder'}
            <li class="subcard">
              <div class="card-head">
                <label for="kind-{i}-{j}">{t('delivery.kind')}</label>
                <select id="kind-{i}-{j}" value={kind} onchange={(e) => setKind(i, j, (e.currentTarget as HTMLSelectElement).value as DeliveryKind)}>
                  {#each DELIVERY_KINDS as k (k)}<option value={k}>{t(`delivery.kind_value.${k}`)}</option>{/each}
                </select>
                <button type="button" class="small danger" onclick={() => set(`routes[${i}].deliveries`, deliveries(r).filter((_, k) => k !== j))}>{t('config.remove_delivery')}</button>
              </div>
              {#each DELIVERY_FIELDS[kind] ?? [] as f (f.key)}
                <FieldInput field={f} {body} path={joinPath(`routes[${i}].deliveries[${j}]`, f.key)} labels="config.field.target.{kind}" section="" {errors} restartFields={[]} onchange={set} />
              {/each}
            </li>
          {/each}
        </ol>
        <div class="toolbar">
          {#each DELIVERY_KINDS as k (k)}
            <button type="button" class="small" onclick={() => set(`routes[${i}].deliveries`, [...deliveries(r), newDelivery(k)])}>+ {t(`delivery.kind_value.${k}`)}</button>
          {/each}
        </div>
      </li>
    {/each}
  </ol>
  <button type="button" onclick={() => set('routes', [...routes, newRoute()])}>+ {t('config.add_route')}</button>
</div>
