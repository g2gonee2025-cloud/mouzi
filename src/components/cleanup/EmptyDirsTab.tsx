import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { useCleanupStore } from "../../store/useCleanupStore";
import { CLEANUP_REVIEW_LIMIT, type CleanupRequest } from "../../utils/cleanup";
import { Search, Trash2, FolderX } from "lucide-react";
import ResultsPanel from "./ResultsPanel";
import ConfirmDialog from "./ConfirmDialog";
import { useCleanupConfirmation } from "./useCleanupConfirmation";
import UnavailableCleanupRoots from "./UnavailableCleanupRoots";
import { CleanupSelectionControls, useCleanupSelection } from "./CleanupSelection";

const directoryPath = (path: string) => path;

export default function EmptyDirsTab() {
  const { t } = useTranslation();
  const store = useCleanupStore();
  const directories = store.emptyDirs;
  const isCurrent = () => useCleanupStore.getState().emptyDirs === directories;
  const selection = useCleanupSelection(directories, directoryPath, isCurrent);
  const { checked, atLimit } = selection;
  const receipt = store.receipts.remove_empty_dir;
  const { pending, reviewNeedsSearch, clearReviewNotice, openReview } = useCleanupConfirmation();
  const findButton = useRef<HTMLButtonElement>(null);
  const resultsHeading = useRef<HTMLHeadingElement>(null);

  const handleFind = () => {
    clearReviewNotice();
    store.findEmptyDirs();
    selection.clear();
  };

  const handleConfirm = async (opener: HTMLButtonElement) => {
    if (!isCurrent()) return;
    const paths = (directories ?? []).filter((path) => checked.has(path));
    if (!paths.length || paths.length > CLEANUP_REVIEW_LIMIT) return;
    const actions: CleanupRequest[] = paths.map((path) => ({
      kind: "remove_empty_dir",
      path,
    }));
    await openReview(actions, {
      title: t("confirm.op.removeEmptyDirs"),
      notice: "permanent",
      opener,
      focusFallback: () => resultsHeading.current ?? findButton.current,
    });
  };

  const data = store.emptyDirs ?? [];
  const unavailableRoots = store.emptyDirs && store.unavailableRoots.emptyDirs;

  if (receipt && !pending) {
    return (
      <ResultsPanel
        headingRef={resultsHeading}
        results={receipt.outcomes}
        onDone={() => {
          if (!store.dismissReceipt("remove_empty_dir", receipt.planId)) return;
          if (useCleanupStore.getState().loading) selection.clear();
          else handleFind();
        }}
        error={receipt.error}
      />
    );
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs text-text-muted">{t("cleanup.emptyDirsHint")}</p>
        <button
          ref={findButton}
          onClick={handleFind}
          disabled={store.loading || store.busy || !!store.preparedPlan}
          className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-on-primary hover:bg-primary-hover disabled:opacity-50 transition-colors"
        >
          <Search size={14} />
          {store.loading ? t("app.loading") : t("cleanup.findEmptyDirs")}
        </button>
      </div>

      {reviewNeedsSearch && <p role="status" className="text-xs text-text-muted">{t("cleanup.reviewChanged")}</p>}
      {store.error && (
        <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-xs text-red-700 dark:text-red-400">
          {store.error}
        </div>
      )}

      <UnavailableCleanupRoots paths={unavailableRoots ?? undefined} noMatches={data.length === 0} />

      {store.emptyDirs && data.length === 0 && !unavailableRoots?.length && (
        <div className="rounded-lg border border-border bg-surface-dark p-6 text-center text-xs text-text-muted">
          {t("cleanup.noEmptyDirs")}
        </div>
      )}

      {data.length > 0 && (
        <>
          <div className="flex flex-wrap items-start justify-between gap-2 px-1">
            <CleanupSelectionControls selection={selection} />
            <button
              onClick={(event) => void handleConfirm(event.currentTarget)}
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
                    aria-label={path}
                    checked={checked.has(path)}
                    disabled={atLimit && !checked.has(path)}
                    onChange={() => selection.toggle(path)}
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
