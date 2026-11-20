import { useState, useCallback, useId, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useCleanupStore } from "../../store/useCleanupStore";
import { formatBytes, formatTimestamp } from "../../utils/format";
import type { CleanupRequest } from "../../utils/cleanup";
import { Search, Trash2, ExternalLink } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import ResultsPanel from "./ResultsPanel";
import ConfirmDialog, { type ConfirmDialogProps } from "./ConfirmDialog";
import { usePreviewOwner } from "./usePreviewOwner";
import UnavailableCleanupRoots from "./UnavailableCleanupRoots";

const BYTES_PER_MIB = 1_048_576;

interface FileListTabProps {
  kind: "large" | "stale";
  /** Default threshold in MiB (for large) or days (for stale). */
  defaultThreshold: number;
  /** Label for the find button. */
  findLabel: string;
  /** Hint text shown above the find button. */
  hint: string;
  /** Translation key for the empty state. */
  emptyKey: string;
  /** Operation name shown as the dialog title and confirm-button label. */
  operationKey: string;
}

export default function FileListTab({
  kind,
  defaultThreshold,
  findLabel,
  hint,
  emptyKey,
  operationKey,
}: FileListTabProps) {
  const { t } = useTranslation();
  const store = useCleanupStore();
  const thresholdId = useId();
  const [draftThreshold, setDraftThreshold] = useState<number | null>(null);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [showResults, setShowResults] = useState(false);
  const [pending, setPending] = useState<ConfirmDialogProps | null>(null);
  const beginPreview = usePreviewOwner();
  const findButton = useRef<HTMLButtonElement>(null);
  const resultsHeading = useRef<HTMLHeadingElement>(null);

  const finderKey = kind === "large" ? "largeFiles" : "staleFiles";
  const search = store[finderKey];
  const searchedThreshold = search && ("minBytes" in search ? search.minBytes / BYTES_PER_MIB : search.days);
  const threshold = draftThreshold ?? searchedThreshold ?? defaultThreshold;
  const data = search && threshold === searchedThreshold ? search.files : null;
  const unavailableRoots = data && store.unavailableRoots[finderKey];

  const checkedFiles = useMemo(
    () => (data ?? []).filter((f) => checked.has(f.path)),
    [data, checked],
  );

  const handleFind = () => {
    setDraftThreshold(threshold);
    if (kind === "large") {
      store.findLargeFiles(threshold * BYTES_PER_MIB);
    } else {
      store.findStaleFiles(threshold);
    }
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
    if (!data) return;
    if (checked.size === data.length) {
      setChecked(new Set());
    } else {
      setChecked(new Set(data.map((f) => f.path)));
    }
  }, [data, checked]);

  const handleConfirm = async (opener: HTMLButtonElement) => {
    if (!search || checkedFiles.length === 0 || useCleanupStore.getState()[finderKey] !== search) return;
    const actions = checkedFiles.map(({ path }): CleanupRequest => "minBytes" in search
      ? { kind: "trash_large", path, minBytes: search.minBytes }
      : { kind: "trash_stale", path, days: search.days },
    );
    const owner = beginPreview();
    if (!owner) return;
    try {
      const plan = await store.prepareCleanup(actions, owner.signal);
      if (owner.signal.aborted) return;
      setPending({
        title: t(operationKey),
        count: plan.actions.length,
        bytes: plan.totalBytes,
        notice: "trash",
        preview: plan.actions.map((f) => f.path),
        opener,
        focusFallback: () => resultsHeading.current ?? findButton.current,
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

  const handleOpen = async (path: string) => {
    try {
      await invoke("open_folder_cmd", { path });
    } catch (error) {
      useCleanupStore.setState({ error: String(error) });
    }
  };

  if (showResults && store.results) {
    return (
      <ResultsPanel
        headingRef={resultsHeading}
        results={store.results}
        onDone={handleResultsDone}
        error={store.error}
      />
    );
  }

  const isBytes = kind === "large";

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-3">
        <div className="flex-1">
          <p className="text-xs text-text-muted">{hint}</p>
        </div>
        <div className="flex items-center gap-2">
          <label htmlFor={thresholdId} className="text-xs text-text-muted shrink-0">
            {isBytes
              ? t("cleanup.minBytes")
              : t("cleanup.staleDays")}
          </label>
          <input
            id={thresholdId}
            type="number"
            min={1}
            value={threshold}
            onChange={(event) => {
              setDraftThreshold(Math.max(1, Number(event.target.value)));
              setChecked(new Set());
            }}
            className="w-20 rounded-md border border-border bg-surface px-2 py-1 text-xs text-text"
          />
          <button
            ref={findButton}
            onClick={handleFind}
          disabled={store.loading || store.busy || !!store.preparedPlan}
            className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-on-primary hover:bg-primary-hover disabled:opacity-50 transition-colors"
          >
            <Search size={14} />
            {store.loading ? t("app.loading") : findLabel}
          </button>
        </div>
      </div>

      {store.error && (
        <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-xs text-red-700 dark:text-red-400">
          {store.error}
        </div>
      )}

      {!store.loading && search && !data && (
        <p className="text-xs text-text-muted">{t("cleanup.notSearched")}</p>
      )}

      <UnavailableCleanupRoots paths={unavailableRoots ?? undefined} noMatches={data?.length === 0} />

      {data && data.length === 0 && !unavailableRoots?.length && (
        <div className="rounded-lg border border-border bg-surface-dark p-6 text-center text-xs text-text-muted">
          {t(emptyKey)}
        </div>
      )}

      {data && data.length > 0 && (
        <>
          <div className="flex items-center justify-between px-1">
            <label className="flex items-center gap-2 text-xs text-text-muted cursor-pointer">
              <input
                type="checkbox"
                checked={data.length > 0 && checked.size === data.length}
                onChange={toggleAll}
                className="rounded border-border"
              />
              {t("cleanup.selectAll")} ({data.length})
            </label>
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
              {data.map((f) => {
                const fname =
                  f.path.split("\\").pop()?.split("/").pop() || f.path;
                return (
                  <div
                    key={f.path}
                    className="flex items-center gap-2 px-3 py-2 hover:bg-surface transition-colors border-b border-border last:border-0 group"
                  >
                    <input
                      type="checkbox"
                      aria-label={f.path}
                      checked={checked.has(f.path)}
                      onChange={() => toggle(f.path)}
                      className="rounded border-border shrink-0"
                    />
                    <div className="flex-1 min-w-0">
                      <div className="text-xs text-text truncate">{fname}</div>
                      <div className="text-[10px] text-text-muted truncate">
                        {f.path}
                      </div>
                    </div>
                    <span className="text-xs text-text-muted shrink-0">
                      {formatBytes(f.size)}
                    </span>
                    <span className="text-[10px] text-text-muted shrink-0 hidden sm:inline">
                      {formatTimestamp(f.mtime)}
                    </span>
                    <button
                      onClick={() => handleOpen(f.path)}
                      className="p-1 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 transition-all"
                      title={t("cleanup.openFolder")}
                    >
                      <ExternalLink size={12} />
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        </>
      )}

      {pending && <ConfirmDialog {...pending} />}
    </div>
  );
}
