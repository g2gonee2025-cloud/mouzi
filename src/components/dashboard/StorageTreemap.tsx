import { useMemo } from "react";
import { useTranslation } from "react-i18next";

interface StorageTreemapProps {
  data: Array<{ path: string; files: number; bytes: number }>;
  totalBytes: number;
}

// Simple slice-and-dice treemap: sort by bytes descending, allocate proportional width
export default function StorageTreemap({ data, totalBytes }: StorageTreemapProps) {
  const { t } = useTranslation();

  const sorted = useMemo(
    () => [...data].sort((a, b) => b.bytes - a.bytes),
    [data]
  );

  if (sorted.length === 0 || totalBytes === 0) {
    return (
      <div className="rounded-lg border border-border bg-surface-dark p-4">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.storageDistribution")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      </div>
    );
  }

  const colors = [
    "#f59e0b", "#3b82f6", "#10b981", "#ef4444",
    "#8b5cf6", "#ec4899", "#14b8a6", "#f97316",
  ];

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-4">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.storageDistribution")}</h3>
      <svg
        viewBox="0 0 400 200"
        className="w-full"
        role="img"
        aria-label={t("dashboard.storageDistribution")}
      >
        {sorted.map((item, i) => {
          const pct = totalBytes > 0 ? item.bytes / totalBytes : 0;
          const x = sorted.slice(0, i).reduce((sum, r) => {
            const rpct = totalBytes > 0 ? r.bytes / totalBytes : 0;
            return sum + rpct * 400;
          }, 0);
          const w = pct * 400;
          // Minimum 2px width for visibility
          const fill = colors[i % colors.length];
          const label = item.path.replace(/\\/g, "/").split("/").pop() || item.path;
          return (
            <g key={item.path}>
              <rect x={x} y={0} width={Math.max(w, 2)} height={200} fill={fill} rx={4} />
              {w > 40 && (
                <>
                  <text
                    x={x + w / 2}
                    y={90}
                    textAnchor="middle"
                    fill="white"
                    fontSize="11"
                    fontWeight="600"
                  >
                    {label}
                  </text>
                  <text
                    x={x + w / 2}
                    y={110}
                    textAnchor="middle"
                    fill="rgba(255,255,255,0.8)"
                    fontSize="9"
                  >
                    {Math.round(pct * 100)}%
                  </text>
                </>
              )}
            </g>
          );
        })}
      </svg>
      <div className="flex flex-wrap gap-3 mt-3">
        {sorted.map((item, i) => {
          const pct = totalBytes > 0 ? Math.round((item.bytes / totalBytes) * 100) : 0;
          const label = item.path.replace(/\\/g, "/").split("/").pop() || item.path;
          return (
            <div key={item.path} className="flex items-center gap-1.5 text-xs">
              <span
                className="inline-block w-2.5 h-2.5 rounded-sm"
                style={{ backgroundColor: colors[i % colors.length] }}
              />
              <span className="text-text-muted">{label}</span>
              <span className="text-text font-medium">{pct}%</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}