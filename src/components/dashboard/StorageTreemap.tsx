import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { FolderOpen } from "lucide-react";
import { formatBytes } from "../../utils/format";
import { fileName } from "../../utils/paths";
import { formatShare, percentOf } from "../../utils/dashboard";

interface StorageTreemapProps {
  data: Array<{ path: string; files: number; bytes: number }>;
  totalBytes: number;
  selected?: string | null;
  onSelect?: (path: string | null) => void;
}

export default function StorageTreemap({
  data,
  totalBytes,
  selected,
  onSelect,
}: StorageTreemapProps) {
  const { t } = useTranslation();

  const sorted = useMemo(
    () => [...data].sort((a, b) => b.bytes - a.bytes),
    [data],
  );

  if (sorted.length === 0 || totalBytes === 0) {
    return (
      <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.watchedRoots")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      </div>
    );
  }

  const openRoot = async (path: string) => {
    try {
      await invoke("open_folder_cmd", { path });
    } catch {
      // ignore
    }
  };

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4 h-full">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.watchedRoots")}</h3>
      <div className="space-y-2">
        {sorted.map((item) => {
          const pct = percentOf(item.bytes, totalBytes);
          const isOn = selected === item.path;
          const label = fileName(item.path) || item.path;
          return (
            <div
              key={item.path}
              className={`rounded-md px-2 py-1.5 -mx-1 transition-colors ${
                isOn ? "bg-surface" : "hover:bg-surface/60"
              }`}
            >
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  className="flex-1 min-w-0 text-left"
                  aria-pressed={isOn}
                  onClick={() => onSelect?.(isOn ? null : item.path)}
                  title={item.path}
                >
                  <div className="flex items-center justify-between text-xs mb-1 gap-2">
                    <span className="text-text font-medium truncate">{label}</span>
                    <span className="text-text-muted tabular-nums shrink-0">
                      {formatShare(pct)} · {formatBytes(item.bytes)}
                    </span>
                  </div>
                  <div className="h-1.5 bg-border rounded-full overflow-hidden">
                    <div
                      className="h-full rounded-full bg-primary"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <div className="text-[10px] text-text-muted mt-1 truncate">
                    {item.files.toLocaleString()} {t("dashboard.filesLabel")}
                  </div>
                </button>
                <button
                  type="button"
                  onClick={() => openRoot(item.path)}
                  aria-label={`${t("dashboard.openFolder")}: ${label}`}
                  className="inline-flex items-center justify-center w-6 h-6 rounded text-text-muted hover:text-text hover:bg-border shrink-0 focus-visible:outline-2 focus-visible:outline-primary focus-visible:outline-offset-1"
                  title={t("dashboard.openFolder")}
                >
                  <FolderOpen size={13} aria-hidden="true" />
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
