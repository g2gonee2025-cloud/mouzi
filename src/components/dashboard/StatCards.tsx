import { useTranslation } from "react-i18next";
import { formatBytes, formatTimestamp } from "../../utils/format";
import { ScanProgress } from "../../store/useDashboardStore";
import { HardDrive, FileText, FolderTree, Clock, RefreshCw } from "lucide-react";

interface StatCardsProps {
  totalFiles: number;
  totalBytes: number;
  watchedRoots: number;
  lastScanAt: number | null;
  isScanning: boolean;
  scanProgress: ScanProgress | null;
  onScan: () => void;
}

export default function StatCards({
  totalFiles,
  totalBytes,
  watchedRoots,
  lastScanAt,
  isScanning,
  scanProgress,
  onScan,
}: StatCardsProps) {
  const { t } = useTranslation();

  const cards = [
    {
      icon: <FileText size={18} className="text-primary" />,
      label: t("dashboard.totalFiles"),
      value: totalFiles.toLocaleString(),
    },
    {
      icon: <HardDrive size={18} className="text-primary" />,
      label: t("dashboard.totalBytes"),
      value: formatBytes(totalBytes),
    },
    {
      icon: <FolderTree size={18} className="text-primary" />,
      label: t("dashboard.watchedRoots"),
      value: String(watchedRoots),
    },
    {
      icon: <Clock size={18} className="text-primary" />,
      label: t("dashboard.lastScan"),
      value: lastScanAt ? formatTimestamp(lastScanAt) : t("dashboard.neverScanned"),
    },
  ];

  return (
    <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
      {cards.map((card) => (
        <div
          key={card.label}
          className="rounded-lg border border-border bg-surface-dark p-4 flex flex-col gap-2"
        >
          <div className="flex items-center justify-between">
            {card.icon}
          </div>
          <span className="text-2xl font-semibold text-text">{card.value}</span>
          <span className="text-xs text-text-muted">{card.label}</span>
        </div>
      ))}
      <div className="rounded-lg border border-border bg-surface-dark p-4 flex flex-col gap-2 col-span-2 lg:col-span-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <RefreshCw size={16} className={isScanning ? "animate-spin text-primary" : "text-text-muted"} />
            <span className="text-sm font-medium text-text">
              {isScanning ? t("dashboard.scanning") : t("dashboard.scanNow")}
            </span>
          </div>
          <button
            onClick={onScan}
            disabled={isScanning}
            className="rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 transition-colors"
          >
            {isScanning ? t("dashboard.scanning") : t("dashboard.scanNow")}
          </button>
        </div>
        {isScanning && scanProgress && (
          <div className="text-xs text-text-muted">
            {scanProgress.root}: {scanProgress.processed} {t("dashboard.filesProcessed")}
          </div>
        )}
      </div>
    </div>
  );
}