import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/format";
import { categoryColor, leadCategory, percentOf, type CategoryShare } from "../../utils/dashboard";

interface StorageRibbonProps {
  data: CategoryShare[];
  totalBytes: number;
  onSelect?: (category: string) => void;
  selected?: string | null;
}

export default function StorageRibbon({
  data,
  totalBytes,
  onSelect,
  selected,
}: StorageRibbonProps) {
  const { t } = useTranslation();
  const lead = leadCategory(data, totalBytes);
  const sorted = [...data].filter((d) => d.bytes > 0).sort((a, b) => b.bytes - a.bytes);

  if (!sorted.length || totalBytes <= 0) {
    return (
      <div className="rounded-xl border border-border bg-surface-dark px-5 py-4">
        <p className="text-sm text-text-muted">{t("dashboard.leadEmpty")}</p>
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-border bg-surface-dark px-5 py-4">
      <p className="text-[11px] uppercase tracking-[0.16em] text-text-muted mb-1">
        {t("dashboard.storageDistribution")}
      </p>
      <h2 className="text-lg font-semibold text-text leading-snug mb-3">
        {t("dashboard.lead", {
          category: lead?.category ?? "",
          percent: lead?.percent ?? 0,
          size: formatBytes(totalBytes),
        })}
      </h2>
      <div
        className="storage-ribbon"
        role="img"
        aria-label={t("dashboard.storageDistribution")}
      >
        {sorted.map((item) => {
          const pct = percentOf(item.bytes, totalBytes);
          const isOn = selected === item.category;
          return (
            <button
              key={item.category}
              type="button"
              title={`${item.category} · ${Math.round(pct)}% · ${formatBytes(item.bytes)}`}
              onClick={() => onSelect?.(item.category)}
              className="storage-ribbon-seg"
              style={{
                width: `${Math.max(pct, 1.2)}%`,
                backgroundColor: categoryColor(item.category),
                opacity: selected && !isOn ? 0.45 : 1,
              }}
            />
          );
        })}
      </div>
      <div className="flex flex-wrap gap-x-4 gap-y-1.5 mt-3">
        {sorted.map((item) => {
          const pct = Math.round(percentOf(item.bytes, totalBytes));
          const isOn = selected === item.category;
          return (
            <button
              key={item.category}
              type="button"
              onClick={() => onSelect?.(item.category)}
              className={`flex items-center gap-1.5 text-xs rounded-md px-1 -mx-1 py-0.5 transition-colors ${
                isOn ? "bg-border text-text" : "text-text-muted hover:text-text"
              }`}
            >
              <span
                className="inline-block w-2 h-2 rounded-[2px]"
                style={{ backgroundColor: categoryColor(item.category) }}
              />
              <span className="font-medium text-text">{item.category}</span>
              <span>{pct}%</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
