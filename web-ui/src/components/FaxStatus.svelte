<script lang="ts">
  import type { Fax } from '../lib/api';
  import { t } from '../lib/i18n.svelte';

  let { fax }: { fax: Fax } = $props();

  const ICON: Record<string, string> = {
    receiving: '⟳',
    received: '✓',
    partial: '◐',
    failed: '✕',
    no_fax: '–',
  };
</script>

<span class="badge status-{fax.fax_status}">
  <span aria-hidden="true">{ICON[fax.fax_status] ?? '?'}</span>
  {#if fax.fax_status === 'receiving'}
    {t('fax.receiving_page', { page: fax.pages + 1 })}
  {:else}
    {t(`fax.status.${fax.fax_status}`)}
  {/if}
</span>
