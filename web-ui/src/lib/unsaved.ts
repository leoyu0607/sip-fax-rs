// Unsaved changes: a form says whether it has any; leaving the page then asks first (a link in the
// interface: a confirm; closing or reloading the tab: the browser's own question).

const guards = new Set<() => boolean>();

/** `isDirty` is asked when the page is about to change; returns the unregister. */
export function guardUnsaved(isDirty: () => boolean): () => void {
  guards.add(isDirty);
  return () => guards.delete(isDirty);
}

export function hasUnsaved(): boolean {
  for (const g of guards) if (g()) return true;
  return false;
}

/** For `beforeunload`: asks the browser to confirm when something is unsaved. */
export function onBeforeUnload(e: BeforeUnloadEvent): void {
  if (!hasUnsaved()) return;
  e.preventDefault();
  // (older browsers want a returnValue)
  e.returnValue = '';
}
