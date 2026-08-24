import { useMemo } from "react";
import { useTranslation } from "react-i18next";

interface ActivityTimelineProps {
  data: Array<{ file_type: string; count: number }>;
}

export default function ActivityTimeline({ data }: ActivityTimelineProps) {
  const { t } = useTranslation();

  const maxCount = useMemo(
    () => Math.max(...data.map((d) => d.count), 1),
    [data]
  );

  if (data.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-surface-dark p-4">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.activityTimeline")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      </div>
    );
  }

  // Simple bar chart: each category is a bar
  const barWidth = Math.max(20, Math.min(60, 320 / data.length));
  const chartHeight = 120;
  const svgWidth = Math.max(320, data.length * (barWidth + 8));

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-4">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.activityTimeline")}</h3>
      <svg
        viewBox={`0 0 ${svgWidth} ${chartHeight + 30}`}
        className="w-full"
        role="img"
        aria-label={t("dashboard.activityTimeline")}
      >
        {data.map((item, i) => {
          const barH = maxCount > 0 ? (item.count / maxCount) * chartHeight : 0;
          const x = i * (barWidth + 8) + 4;
          const y = chartHeight - barH;
          return (
            <g key={item.file_type}>
              <rect
                x={x}
                y={y}
                width={barWidth}
                height={barH}
                fill="#f59e0b"
                rx={3}
                className="hover:opacity-80 transition-opacity"
              />
              <text
                x={x + barWidth / 2}
                y={chartHeight + 14}
                textAnchor="end"
                fontSize="8"
                fill="#8c7f70"
                transform={`rotate(-45, ${x + barWidth / 2}, ${chartHeight + 14})`}
              >
                {item.file_type.length > 6
                  ? item.file_type.slice(0, 6) + "…"
                  : item.file_type}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}