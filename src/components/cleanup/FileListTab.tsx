import { useState, useCallback, useId, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useCleanupStore } from "../../store/useCleanupStore";
import { formatBytes, formatTimestamp } from "../../utils/format";
import type { CleanupRequest } from "../../utils/cleanup";
import { Search, Trash2, ExternalLink } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import ResultsPanel from "./ResultsPanel";
import ConfirmDialog from "./ConfirmDialog";
import { useCleanupConfirmation } from "./useCleanupConfirmation";
import UnavailableCleanupRoots from "./UnavailableCleanupRoots";

const BYTES_PER_MIB = 1_048_576n;
const MIB_FRACTION_DIGITS = 20;
const MAX_MIN_BYTES = BigInt(Number.MAX_SAFE_INTEGER);
// Rust multiplies days by 86,400 using a checked i64 operation.
const MAX_STALE_DAYS = 9_223_372_036_854_775_807n / 86_400n;

function formatSizeThreshold(minBytes: bigint): string {
  const whole = minBytes / BYTES_PER_MIB;
  const remainder = minBytes % BYTES_PER_MIB;
  if (remainder === 0n) return String(whole);
  // Multiply the remainder by 5^20 to express remainder / 2^20 in decimal.
  const fraction = String(remainder * 5n ** BigInt(MIB_FRACTION_DIGITS))
    .padStart(MIB_FRACTION_DIGITS, "0").replace(/0+$/, "");
  return whole + "." + fraction;
}

function parseThreshold(raw: string, isBytes: boolean): number | null {
  const match = /^(\d*)(?:\.(\d+))?(?:e([+-]?\d+))?$/i.exec(raw);
  if (!match) return null;
  const [, whole, fraction = "", exponent = "0"] = match;
  const digits = (whole + fraction).replace(/^0+/, "");
  const significant = digits.replace(/0+$/, "");
  const scale = fraction.length - Number(exponent) - (digits.length - significant.length);
  // A safe byte count has at most 16 digits, and a MiB fraction has at most 20.
  if (!significant || !Number.isInteger(scale) || scale > MIB_FRACTION_DIGITS || significant.length - scale > 16) return null;

  const multiplier = isBytes ? BYTES_PER_MIB : 1n;
  const scaled = BigInt(significant) * multiplier;
  const numerator = scale < 0 ? scaled * 10n ** BigInt(-scale) : scaled;
  const divisor = scale > 0 ? 10n ** BigInt(scale) : 1n;
  const criterion = numerator / divisor;
  const maximum = isBytes ? MAX_MIN_BYTES : MAX_STALE_DAYS;
  if (numerator % divisor !== 0n || criterion < multiplier || criterion > maximum) return null;
  return Number(criterion);
}

const MAX_SIZE_THRESHOLD = formatSizeThreshold(MAX_MIN_BYTES);

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
  const [draftThreshold, setDraftThreshold] = useState<string | null>(null);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [showResults, setShowResults] = useState(false);
  const { pending, reviewNeedsSearch, clearReviewNotice, openReview } = useCleanupConfirmation(() => setShowResults(true));
  const findButton = useRef<HTMLButtonElement>(null);
  const resultsHeading = useRef<HTMLHeadingElement>(null);

  const isBytes = kind === "large";
  const finderKey = isBytes ? "largeFiles" : "staleFiles";
  const search = store[finderKey];
  const searchedCriterion = search && ("minBytes" in search ? search.minBytes : search.days);
  const savedThreshold = search && ("minBytes" in search
    ? formatSizeThreshold(BigInt(search.minBytes))
    : String(search.days));
  const threshold = draftThreshold ?? savedThreshold ?? String(defaultThreshold);
  const criterion = parseThreshold(threshold, isBytes);
  const data = search && criterion === searchedCriterion ? search.files : null;
  const unavailableRoots = data && store.unavailableRoots[finderKey];

  const checkedFiles = useMemo(
    () => (data ?? []).filter((f) => checked.has(f.path)),
    [data, checked],
  );

  const handleFind = () => {
    if (criterion === null) return;
    clearReviewNotice();
    setDraftThreshold(threshold);
    if (isBytes) {
      store.findLargeFiles(criterion);
    } else {
      store.findStaleFiles(criterion);
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
    await openReview(actions, {
      title: t(operationKey),
      notice: "trash",
      opener,
      focusFallback: () => resultsHeading.current ?? findButton.current,
    });
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
            max={isBytes ? MAX_SIZE_THRESHOLD : String(MAX_STALE_DAYS)}
            step={isBytes ? "any" : 1}
            value={threshold}
            aria-invalid={criterion === null || undefined}
            onChange={(event) => {
              setDraftThreshold(event.target.value);
              setChecked(new Set());
            }}
            className="w-20 rounded-md border border-border bg-surface px-2 py-1 text-xs text-text"
          />
          <button
            ref={findButton}
            onClick={handleFind}
            disabled={criterion === null || store.loading || store.busy || !!store.preparedPlan}
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

      {reviewNeedsSearch && <p role="status" className="text-xs text-text-muted">{t("cleanup.reviewChanged")}</p>}
      {!reviewNeedsSearch && !store.loading && search && !data && (
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
