import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/format";

interface CategoryBarsProps {
  data: Array<{ category: string; files: number; bytes: number }>;
  totalBytes: number;
}

const COLORS = [
  "#f59e0b", "#3b82f6", "#10b981", "#ef4444",
  "#8b5cf6", "#ec4899", "#14b8a6", "#f97316",
  "#6366f1", "#84cc16",
];

export default function CategoryBars({ data, totalBytes }: CategoryBarsProps) {
  const { t } = useTranslation();

  const sorted = useMemo(
    () => [...data].sort((a, b) => b.bytes - a.bytes),
    [data]
  );

  if (sorted.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-surface-dark p-4">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.categoryBreakdown")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-4">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.categoryBreakdown")}</h3>
      <div className="space-y-2">
        {sorted.map((item, i) => {
          const pct = totalBytes > 0 ? (item.bytes / totalBytes) * 100 : 0;
          return (
            <div key={item.category}>
              <div className="flex items-center justify-between text-xs mb-1">
                <div className="flex items-center gap-1.5">
                  <span
                    className="inline-block w-2 h-2 rounded-sm"
                    style={{ backgroundColor: COLORS[i % COLORS.length] }}
                  />
                  <span className="text-text font-medium">{item.category}</span>
                </div>
                <span className="text-text-muted">
                  {item.files.toLocaleString()} {t("dashboard.filesLabel")} · {formatBytes(item.bytes)}
                </span>
              </div>
              <div className="h-2 bg-border rounded-full overflow-hidden">
                <div
                  className="h-full rounded-full transition-all"
                  style={{
                    width: `${Math.max(pct, 1)}%`,
                    backgroundColor: COLORS[i % COLORS.length],
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