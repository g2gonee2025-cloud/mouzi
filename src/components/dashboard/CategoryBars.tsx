import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/format";
import { categoryClass, percentOf } from "../../utils/dashboard";

interface CategoryBarsProps {
  data: Array<{ category: string; files: number; bytes: number }>;
  totalBytes: number;
  selected?: string | null;
  onSelect?: (category: string | null) => void;
}

export default function CategoryBars({
  data,
  totalBytes,
  selected,
  onSelect,
}: CategoryBarsProps) {
  const { t } = useTranslation();

  const sorted = useMemo(
    () => [...data].sort((a, b) => b.bytes - a.bytes),
    [data],
  );

  if (sorted.length === 0) {
    return (
      <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.categoryBreakdown")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.categoryBreakdown")}</h3>
      <div className="space-y-2">
        {sorted.map((item) => {
          const pct = percentOf(item.bytes, totalBytes);
          const isOn = selected === item.category;
          return (
            <button
              key={item.category}
              type="button"
              aria-pressed={isOn}
              onClick={() => onSelect?.(isOn ? null : item.category)}
              className={`w-full text-left rounded-md px-1.5 py-1 -mx-1.5 transition-colors ${
                isOn ? "bg-surface" : "hover:bg-surface/60"
              }`}
            >
              <div className="flex items-center justify-between text-xs mb-1">
                <div className="flex items-center gap-1.5">
                  <span
                    className={`inline-block w-2 h-2 rounded-sm ${categoryClass(item.category)}`}
                    aria-hidden="true"
                  />
                  <span className="text-text font-medium">{item.category}</span>
                </div>
                <span className="text-text-muted tabular-nums">
                  {item.files.toLocaleString()} {t("dashboard.filesLabel")} · {formatBytes(item.bytes)}
                </span>
              </div>
              <div className="h-1.5 bg-border rounded-full overflow-hidden">
                <div
                  className={`h-full rounded-full ${categoryClass(item.category)}`}
                  style={{ width: `${pct}%` }}
                />
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
