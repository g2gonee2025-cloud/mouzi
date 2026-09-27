import { useTranslation } from "react-i18next";
import {
  cleanupStatusMark,
  cleanupSummaryRows,
  type CleanupOutcome,
} from "../../utils/cleanup";
import { RotateCcw } from "lucide-react";

interface ResultsPanelProps {
  results: CleanupOutcome[];
  error: string | null;
  onDone: () => void;
}

export default function ResultsPanel({
  results,
  error,
  onDone,
}: ResultsPanelProps) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-text">
          {t("cleanup.resultsTitle")}
        </h3>
        <button
          onClick={onDone}
          className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-hover transition-colors"
        >
          <RotateCcw size={14} />
          {t("cleanup.backToList")}
        </button>
      </div>

      {error && (
        <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-xs text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <div className="flex flex-wrap gap-2 text-xs">
        {cleanupSummaryRows(results).map((row) => (
          <span key={row.status} className={`rounded-full px-2 py-0.5 ${row.pillClass}`}>
            {t(row.labelKey)}: {row.count}
          </span>
        ))}
      </div>

      <div className="rounded-lg border border-border bg-surface-dark overflow-hidden">
        <div className="max-h-72 overflow-auto">
          {results.map((r) => {
            const fname =
              r.path.split("\\").pop()?.split("/").pop() || r.path;
            const mark = cleanupStatusMark(r.status);
            const Icon = mark.icon;
            return (
              <div
                key={r.path}
                className="flex items-start gap-2 px-3 py-2 border-b border-border last:border-0"
              >
                <Icon size={14} className={`${mark.className} mt-0.5 shrink-0`} />
                <div className="min-w-0">
                  <div className="text-xs text-text truncate">{fname}</div>
                  <div className="text-[10px] text-text-muted truncate">
                    {r.path}
                  </div>
                  {r.message && (
                    <div className="text-[10px] text-text-muted">
                      {r.message}
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}