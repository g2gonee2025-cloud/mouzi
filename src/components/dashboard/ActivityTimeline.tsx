import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { categoryClass, percentOf } from "../../utils/dashboard";
import type { ActivityReadStatus, WeeklyStat } from "../../store/useDashboardStore";

interface ActivityTimelineProps {
  data: WeeklyStat[];
  status?: ActivityReadStatus;
  onRetry?: () => void;
}

export default function ActivityTimeline({ data, status = "ready", onRetry }: ActivityTimelineProps) {
  const { t } = useTranslation();

  const maxCount = useMemo(
    () => Math.max(...data.map((d) => d.count), 1),
    [data],
  );

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.organizedThisWeek")}</h3>
      {status === "loading" && (
        <div role="status" className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.activityLoading")}
        </div>
      )}
      {status === "error" && (
        <div className="flex flex-col items-center justify-center gap-2 h-32 px-2 text-center text-xs">
          <p role="alert" className="text-red-700 dark:text-red-400">{t("dashboard.activityReadFailed")}</p>
          {onRetry && (
            <button type="button" onClick={onRetry} className="rounded-md border border-border px-2 py-1 hover:bg-surface">
              {t("common.retry")}
            </button>
          )}
        </div>
      )}
      {status === "ready" && data.length === 0 && (
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noOrganized")}
        </div>
      )}
      {status === "ready" && data.length > 0 && (
        <div className="space-y-2">
          {data.map((item) => {
            const pct = percentOf(item.count, maxCount);
            return (
              <div key={item.file_type}>
                <div className="flex items-center justify-between text-xs mb-1">
                  <span className="text-text font-medium truncate">{item.file_type}</span>
                  <span className="text-text-muted tabular-nums">
                    {item.count.toLocaleString()}
                  </span>
                </div>
                <div className="h-1.5 bg-border rounded-full overflow-hidden">
                  <div
                    className={`h-full rounded-full ${categoryClass(item.file_type)}`}
                    style={{ width: `${pct}%` }}
                  />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
