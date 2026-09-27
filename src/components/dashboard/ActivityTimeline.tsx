import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { categoryColor } from "../../utils/dashboard";

interface ActivityTimelineProps {
  data: Array<{ file_type: string; count: number }>;
}

export default function ActivityTimeline({ data }: ActivityTimelineProps) {
  const { t } = useTranslation();

  const maxCount = useMemo(
    () => Math.max(...data.map((d) => d.count), 1),
    [data],
  );

  if (data.length === 0) {
    return (
      <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.organizedThisWeek")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noOrganized")}
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.organizedThisWeek")}</h3>
      <div className="space-y-2">
        {data.map((item) => {
          const pct = (item.count / maxCount) * 100;
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
                  className="h-full rounded-full"
                  style={{
                    width: `${Math.max(pct, 4)}%`,
                    backgroundColor: categoryColor(item.file_type),
                  }}
                />
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
