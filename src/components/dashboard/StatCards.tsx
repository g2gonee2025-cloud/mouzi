import { useTranslation } from "react-i18next";
import { formatBytes, formatTimestamp } from "../../utils/format";
import { HardDrive, FileText, FolderTree, Clock } from "lucide-react";

interface StatCardsProps {
  totalFiles: number;
  totalBytes: number;
  watchedRoots: number;
  lastScanAt: number | null;
}

export default function StatCards({
  totalFiles,
  totalBytes,
  watchedRoots,
  lastScanAt,
}: StatCardsProps) {
  const { t } = useTranslation();

  const cards = [
    {
      icon: FileText,
      label: t("dashboard.totalFiles"),
      value: totalFiles.toLocaleString(),
    },
    {
      icon: HardDrive,
      label: t("dashboard.totalBytes"),
      value: formatBytes(totalBytes),
    },
    {
      icon: FolderTree,
      label: t("dashboard.watchedRoots"),
      value: String(watchedRoots),
    },
    {
      icon: Clock,
      label: t("dashboard.lastScan"),
      value: lastScanAt ? formatTimestamp(lastScanAt) : t("dashboard.neverScanned"),
    },
  ];

  return (
    <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
      {cards.map((card) => {
        const Icon = card.icon;
        return (
          <div
            key={card.label}
            className="rounded-xl border border-border bg-surface-dark px-4 py-3 flex flex-col gap-1.5"
          >
            <div className="flex items-center gap-1.5 text-text-muted">
              <Icon size={13} className="text-primary" />
              <span className="text-[11px] uppercase tracking-[0.12em]">{card.label}</span>
            </div>
            <span className="text-xl font-semibold text-text tabular-nums tracking-tight">
              {card.value}
            </span>
          </div>
        );
      })}
    </div>
  );
}
