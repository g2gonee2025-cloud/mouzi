import { useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useCleanupStore } from "../../store/useCleanupStore";
import type { CleanupRequest } from "../../utils/cleanup";
import { Search, Trash2, FolderX } from "lucide-react";
import ResultsPanel from "./ResultsPanel";
import ConfirmDialog, { type ConfirmDialogProps } from "./ConfirmDialog";
import { usePreviewOwner } from "./usePreviewOwner";

export default function EmptyDirsTab() {
  const { t } = useTranslation();
  const store = useCleanupStore();
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [showResults, setShowResults] = useState(false);
  const [pending, setPending] = useState<ConfirmDialogProps | null>(null);
  const beginPreview = usePreviewOwner();

  const handleFind = () => {
    store.findEmptyDirs();
    setChecked(new Set());
    setShowResults(false);
  };

  const toggle = useCallback((path: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  const toggleAll = useCallback(() => {
    const data = store.emptyDirs ?? [];
    if (checked.size === data.length) {
      setChecked(new Set());
    } else {
      setChecked(new Set(data));
    }
  }, [store.emptyDirs, checked]);

  const handleConfirm = async () => {
    const paths = (store.emptyDirs ?? []).filter((path) => checked.has(path));
    if (!paths.length) return;
    const actions: CleanupRequest[] = paths.map((path) => ({
      kind: "remove_empty_dir",
      path,
    }));
    const owner = beginPreview();
    if (!owner) return;
    try {
      const plan = await store.prepareCleanup(actions, owner.signal);
      if (owner.signal.aborted) return;
      setPending({
        title: t("confirm.op.removeEmptyDirs"),
        count: plan.actions.length,
        // `remove_empty_dir` is a bare `fs::remove_dir`, not a trash: there is no
        // byte figure to quote and no Recycle Bin copy to fall back on.
        notice: "permanent",
        preview: plan.actions.map((action) => action.path),
        onConfirm: async () => { await store.executeCleanup(plan.planId); setShowResults(true); },
        onClose: () => { owner.abort(); setPending(null); },
      });
    } catch {
      owner.abort();
    }
  };

  const handleResultsDone = () => {
    setShowResults(false);
    store.clearResults();
    handleFind();
  };

  const data = store.emptyDirs ?? [];

  if (showResults && store.results) {
    return (
      <ResultsPanel
        results={store.results}
        onDone={handleResultsDone}
        error={store.error}
      />
    );
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs text-text-muted">{t("cleanup.emptyDirsHint")}</p>
        <button
          onClick={handleFind}
          disabled={store.loading || store.busy || !!store.preparedPlan}
          className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-on-primary hover:bg-primary-hover disabled:opacity-50 transition-colors"
        >
          <Search size={14} />
          {store.loading ? t("app.loading") : t("cleanup.findEmptyDirs")}
        </button>
      </div>

      {store.error && (
        <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-xs text-red-700 dark:text-red-400">
          {store.error}
        </div>
      )}

      {data.length === 0 && (
        <div className="rounded-lg border border-border bg-surface-dark p-6 text-center text-xs text-text-muted">
          {t("cleanup.noEmptyDirs")}
        </div>
      )}

      {data.length > 0 && (
        <>
          <div className="flex items-center justify-between px-1">
            <label className="flex items-center gap-2 text-xs text-text-muted cursor-pointer">
              <input
                type="checkbox"
                checked={checked.size === data.length}
                onChange={toggleAll}
                className="rounded border-border"
              />
              {t("cleanup.selectAll")} ({data.length})
            </label>
            <button
              onClick={handleConfirm}
              disabled={store.busy || checked.size === 0}
              className="flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-700 disabled:opacity-50 transition-colors"
            >
              <Trash2 size={14} />
              {store.busy
                ? t("app.loading")
                : t("cleanup.confirm", { count: checked.size })}
            </button>
          </div>

          <div className="rounded-lg border border-border bg-surface-dark overflow-hidden">
            <div className="max-h-80 overflow-auto">
              {data.map((path) => (
                <div
                  key={path}
                  className="flex items-center gap-2 px-3 py-2 hover:bg-surface transition-colors border-b border-border last:border-0"
                >
                  <input
                    type="checkbox"
                    checked={checked.has(path)}
                    onChange={() => toggle(path)}
                    className="rounded border-border shrink-0"
                  />
                  <FolderX size={14} className="text-text-muted shrink-0" />
                  <span className="text-xs text-text truncate">{path}</span>
                </div>
              ))}
            </div>
          </div>
        </>
      )}

      {pending && <ConfirmDialog {...pending} />}
    </div>
  );
}
