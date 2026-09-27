import { useEffect, useState } from "react";
import { parseHash, navigateHash } from "../utils/paths";
import { useTranslation } from "react-i18next";
import { Sparkles, ChevronLeft, History, Info } from "lucide-react";
import { useCleanupStore } from "../store/useCleanupStore";
import DuplicatesTab from "../components/cleanup/DuplicatesTab";
import FileListTab from "../components/cleanup/FileListTab";
import EmptyDirsTab from "../components/cleanup/EmptyDirsTab";
import HistoryPanel from "../components/cleanup/HistoryPanel";

type Tab = "duplicates" | "large" | "stale" | "emptyDirs" | "history";

function tabFromHash(): Tab {
  const raw = parseHash().params.get("tab");
  if (raw === "duplicates" || raw === "large" || raw === "stale" || raw === "emptyDirs" || raw === "history") {
    return raw;
  }
  return "duplicates";
}

export default function Cleanup() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>(tabFromHash);

  useEffect(() => {
    const onHash = () => setTab(tabFromHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const hasAnyData = useCleanupStore((s) =>
    s.duplicates !== null ||
    s.largeFiles !== null ||
    s.staleFiles !== null ||
    s.emptyDirs !== null ||
    s.history !== null,
  );

  const handleBack = () => {
    navigateHash("dashboard");
  };

  const tabs: { id: Tab; label: string }[] = [
    { id: "duplicates", label: t("cleanup.tabs.duplicates") },
    { id: "large", label: t("cleanup.tabs.large") },
    { id: "stale", label: t("cleanup.tabs.stale") },
    { id: "emptyDirs", label: t("cleanup.tabs.emptyDirs") },
    { id: "history", label: t("cleanup.tabs.history") },
  ];

  return (
    <div className="h-full flex flex-col bg-surface text-text">
      {/* Header */}
      <header className="flex items-center justify-between px-5 py-3 border-b border-border bg-surface-dark shrink-0">
        <div className="flex items-center gap-2">
          <button
            onClick={handleBack}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={18} />
          </button>
          <Sparkles size={18} className="text-primary" />
          <h1 className="text-base font-semibold">{t("cleanup.title")}</h1>
        </div>
        {tab !== "history" && (
          <button
            onClick={() => setTab("history")}
            className="flex items-center gap-1 text-xs text-text-muted hover:text-text transition-colors"
          >
            <History size={14} />
            {t("cleanup.tabs.history")}
          </button>
        )}
      </header>

      {/* Tab bar */}
      <div className="flex border-b border-border bg-surface-dark shrink-0">
        {tabs.slice(0, 4).map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={`px-4 py-2 text-xs font-medium transition-colors border-b-2 ${
              tab === t.id
                ? "border-primary text-text"
                : "border-transparent text-text-muted hover:text-text"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto p-4">
        {!hasAnyData && (
          <div className="mb-3 rounded-lg border border-border bg-surface-dark px-4 py-3 text-xs text-text-muted flex items-center gap-2">
            <Info size={14} className="text-primary shrink-0" />
            <span>{t("cleanup.scanFirstHint")}</span>
          </div>
        )}

        {tab === "duplicates" && <DuplicatesTab />}
        {tab === "large" && (
          <FileListTab
            kind="large"
            defaultThreshold={100}
            findLabel={t("cleanup.findLarge")}
            hint={t("cleanup.largeHint")}
            emptyKey="cleanup.noLargeFiles"
          />
        )}
        {tab === "stale" && (
          <FileListTab
            kind="stale"
            defaultThreshold={365}
            findLabel={t("cleanup.findStale")}
            hint={t("cleanup.staleHint")}
            emptyKey="cleanup.noStaleFiles"
          />
        )}
        {tab === "emptyDirs" && <EmptyDirsTab />}
        {tab === "history" && <HistoryPanel />}
      </div>
    </div>
  );
}