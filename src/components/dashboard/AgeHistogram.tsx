import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/format";
import type { AgeBucket } from "../../store/useDashboardStore";
import { AGE_BUCKETS } from "../../utils/dashboard";

interface AgeHistogramProps {
  data: AgeBucket[];
}

export default function AgeHistogram({ data }: AgeHistogramProps) {
  const { t } = useTranslation();
  const labels: Record<string, string> = {
    "7d": t("dashboard.age7d"),
    "30d": t("dashboard.age30d"),
    "90d": t("dashboard.age90d"),
    "365d": t("dashboard.age365d"),
    older: t("dashboard.ageOlder"),
  };

  const ordered = AGE_BUCKETS.map(
    (id) => data.find((d) => d.bucket === id) ?? { bucket: id, files: 0, bytes: 0 },
  );
  const max = Math.max(...ordered.map((d) => d.files), 1);
  const empty = ordered.every((d) => d.files === 0);

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.ageTitle")}</h3>
      {empty ? (
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      ) : (
        <div className="flex items-end gap-2 h-36">
          {ordered.map((bucket) => {
            const h = (bucket.files / max) * 100;
            return (
              <div key={bucket.bucket} className="flex-1 flex flex-col items-center gap-1.5 min-w-0">
                <div className="text-[10px] text-text-muted tabular-nums">
                  {bucket.files ? bucket.files.toLocaleString() : ""}
                </div>
                <div className="w-full h-24 bg-border/60 rounded-sm flex items-end overflow-hidden">
                  <div
                    className="w-full bg-primary rounded-sm"
                    style={{ height: `${Math.max(h, bucket.files > 0 ? 4 : 0)}%` }}
                    title={`${labels[bucket.bucket]} · ${bucket.files.toLocaleString()} · ${formatBytes(bucket.bytes)}`}
                  />
                </div>
                <div className="text-[10px] text-text-muted truncate w-full text-center">
                  {labels[bucket.bucket]}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
