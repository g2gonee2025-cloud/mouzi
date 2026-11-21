import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { CLEANUP_REVIEW_LIMIT } from "../../utils/cleanup";

const emptySelection: ReadonlySet<string> = new Set();

function hasOutsideSelection(checked: ReadonlySet<string>, firstBatch: ReadonlySet<string>) {
  for (const path of checked) {
    if (!firstBatch.has(path)) return true;
  }
  return false;
}

export function useCleanupSelection<Item>(
  items: readonly Item[] | null,
  getPath: (item: Item) => string,
  isCurrent: () => boolean,
) {
  const [selection, setSelection] = useState<{ items: readonly Item[]; paths: ReadonlySet<string> } | null>(null);
  const checked = selection?.items === items ? selection.paths : emptySelection;
  const firstBatch = useMemo(
    () => new Set(items?.slice(0, CLEANUP_REVIEW_LIMIT).map(getPath)),
    [items, getPath],
  );
  const count = items?.length ?? 0;
  const oversized = count > CLEANUP_REVIEW_LIMIT;

  function update(change: (current: ReadonlySet<string>) => ReadonlySet<string>) {
    if (!items || !isCurrent()) return;
    setSelection((previous) => {
      const current = previous?.items === items ? previous.paths : emptySelection;
      const paths = change(current);
      return current === paths ? previous : { items, paths };
    });
  }

  return {
    count,
    checked,
    oversized,
    outsideBatch: oversized && hasOutsideSelection(checked, firstBatch),
    atLimit: checked.size >= CLEANUP_REVIEW_LIMIT,
    toggle(path: string) {
      update((current) => {
        const wasSelected = current.has(path);
        if (!wasSelected && current.size >= CLEANUP_REVIEW_LIMIT) return current;
        const next = new Set(current);
        if (wasSelected) next.delete(path);
        else next.add(path);
        return next;
      });
    },
    toggleAll() {
      if (!oversized) update((current) => current.size === count ? emptySelection : firstBatch);
    },
    selectFirstBatch() {
      if (oversized) update((current) => hasOutsideSelection(current, firstBatch) ? current : firstBatch);
    },
    clear() {
      setSelection(null);
    },
  };
}

export function CleanupSelectionControls({ selection }: { selection: ReturnType<typeof useCleanupSelection> }) {
  const { t } = useTranslation();
  if (!selection.oversized) {
    return (
      <label className="flex items-center gap-2 text-xs text-text-muted cursor-pointer">
        <input type="checkbox" checked={selection.checked.size === selection.count}
          onChange={selection.toggleAll} className="rounded border-border" />
        {t("cleanup.selectAll")} ({selection.count})
      </label>
    );
  }

  const limit = CLEANUP_REVIEW_LIMIT;
  const buttonClass = "rounded border border-border px-2 py-1 hover:bg-surface-dark disabled:opacity-50";
  return (
    <div className="space-y-1 text-xs text-text-muted">
      <p>{t("cleanup.selectionLimit", { limit })}</p>
      <div className="flex flex-wrap gap-2">
        <button type="button" onClick={selection.selectFirstBatch}
          disabled={selection.atLimit || selection.outsideBatch} className={buttonClass}>
          {t("cleanup.selectFirstBatch", { limit })}
        </button>
        <button type="button" onClick={selection.clear} disabled={selection.checked.size === 0}
          className={buttonClass}>
          {t("beta.clearSelection")}
        </button>
        <span className="self-center" aria-live="polite">{t("beta.selected", { count: selection.checked.size })}</span>
      </div>
      {selection.outsideBatch && <p role="status">{t("cleanup.selectionOutsideBatch", { limit })}</p>}
    </div>
  );
}
