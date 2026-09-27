import { useTranslation } from "react-i18next";
import { HardDrive, Clock, Copy, Sparkles } from "lucide-react";
import { formatBytes } from "../../utils/format";
import type { DashboardInsights } from "../../store/useDashboardStore";

interface InsightCardsProps {
  insights: DashboardInsights;
  onOpen: (dest: "large" | "stale" | "duplicates" | "suggestions") => void;
}

export default function InsightCards({ insights, onOpen }: InsightCardsProps) {
  const { t } = useTranslation();

  const cards = [
    {
      key: "large" as const,
      icon: HardDrive,
      title: t("dashboard.largeFiles"),
      count: insights.largeFiles,
      detail: formatBytes(insights.largeBytes),
      hint: t("dashboard.largeHint"),
    },
    {
      key: "stale" as const,
      icon: Clock,
      title: t("dashboard.staleFiles"),
      count: insights.staleFiles,
      detail: formatBytes(insights.staleBytes),
      hint: t("dashboard.staleHint"),
    },
    {
      key: "duplicates" as const,
      icon: Copy,
      title: t("dashboard.possibleDuplicates"),
      count: insights.sameSizeGroups,
      detail: formatBytes(insights.sameSizeExtraBytes),
      hint: t("dashboard.duplicatesHint"),
    },
    {
      key: "suggestions" as const,
      icon: Sparkles,
      title: t("dashboard.uncategorized"),
      count: insights.otherFiles,
      detail: t("dashboard.filesLabel"),
      hint: t("dashboard.uncategorizedHint"),
    },
  ];

  return (
    <div>
      <p className="text-[11px] uppercase tracking-[0.16em] text-text-muted mb-2">
        {t("dashboard.insights")}
      </p>
      <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
        {cards.map((card) => {
          const Icon = card.icon;
          const empty = card.count === 0;
          return (
            <button
              key={card.key}
              type="button"
              onClick={() => onOpen(card.key)}
              className="text-left rounded-xl border border-border bg-surface-dark p-3.5 hover:border-primary/50 hover:bg-surface transition-colors group"
            >
              <div className="flex items-center justify-between mb-2">
                <Icon size={15} className="text-primary" />
                <span className="text-[10px] text-text-muted opacity-0 group-hover:opacity-100 transition-opacity">
                  {t("dashboard.open")}
                </span>
              </div>
              <div className="text-xl font-semibold tabular-nums text-text">
                {card.count.toLocaleString()}
              </div>
              <div className="text-xs font-medium text-text mt-0.5">{card.title}</div>
              <div className="text-[11px] text-text-muted mt-1">
                {empty ? t("dashboard.insightClear") : `${card.detail} · ${card.hint}`}
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
