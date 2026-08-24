import { useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useCleanupStore } from "../../store/useCleanupStore";
import {
  formatBytes,
  formatTimestamp,
} from "../../utils/format";
import {
  reclaimableBytes,
  sortGroups,
  buildDuplicateActions,
  type DuplicateGroup,
} from "../../utils/cleanup";
import { Search, Trash2, Check } from "lucide-react";
import ResultsPanel from "./ResultsPanel";

export default function DuplicatesTab() {
  const { t } = useTranslation();
  const store = useCleanupStore();
  const [selected, setSelected] = useState<Record<string, string>>({});
  const [showResults, setShowResults] = useState(false);

  const groups = useMemo(
    () => (store.duplicates ? sortGroups(store.duplicates) : []),
    [store.duplicates],
  );
  const reclaimable = useMemo(() => reclaimableBytes(groups), [groups]);

  const handleFind = () => store.findDuplicates();

  const handleKeep = (hash: string, path: string) => {
    setSelected((prev) => ({ ...prev, [hash]: path }));
  };

  const handleConfirm = async () => {
    const actions = buildDuplicateActions(groups, selected);
    await store.executeCleanup(actions);
    setShowResults(true);
  };

  // When results come back, refresh the list
  const handleResultsDone = () => {
    setShowResults(false);
    store.clearResults();
    store.findDuplicates();
  };

  if (showResults && store.results) {
    return (
      <ResultsPanel
        results={store.results}
        onDone={handleResultsDone}
        error={store.error}
      />
    );
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs text-text-muted">{t("cleanup.duplicatesHint")}</p>
        <button
          onClick={handleFind}
          disabled={store.loading}
          className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-hover disabled:opacity-50 transition-colors"
        >
          <Search size={14} />
          {store.loading
            ? t("app.loading")
            : t("cleanup.findDuplicates")}
        </button>
      </div>

      {store.error && (
        <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-xs text-red-700 dark:text-red-400">
          {store.error}
        </div>
      )}

      {store.duplicates && groups.length === 0 && (
        <div className="rounded-lg border border-border bg-surface-dark p-6 text-center text-xs text-text-muted">
          {t("cleanup.noDuplicates")}
        </div>
      )}

      {groups.length > 0 && (
        <>
          <div className="flex items-center justify-between px-1">
            <span className="text-xs text-text-muted">
              {groups.length} {t("cleanup.groupCount")} &middot;{" "}
              {t("cleanup.reclaimable", { size: formatBytes(reclaimable) })}
            </span>
            <button
              onClick={handleConfirm}
              disabled={store.busy}
              className="flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-700 disabled:opacity-50 transition-colors"
            >
              <Trash2 size={14} />
              {store.busy
                ? t("app.loading")
                : t("cleanup.confirm", {
                    count: groups.reduce<number>(
                      (sum, g) =>
                        sum +
                        g.files.filter(
                          (f) => f.path !== selected[g.hash],
                        ).length,
                      0,
                    ),
                  })}
            </button>
          </div>

          <div className="space-y-2">
            {groups.map((g) => (
              <DuplicateGroupCard
                key={g.hash}
                group={g}
                keepPath={selected[g.hash] ?? g.files[0]?.path}
                onKeep={handleKeep}
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function DuplicateGroupCard({
  group,
  keepPath,
  onKeep,
}: {
  group: DuplicateGroup;
  keepPath: string;
  onKeep: (hash: string, path: string) => void;
}) {
  const { t } = useTranslation();
  const hashShort = group.hash.slice(0, 12);

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-3">
      <div className="flex items-center gap-2 mb-2">
        <span className="text-[10px] font-mono text-text-muted bg-surface px-1.5 py-0.5 rounded">
          {hashShort}…
        </span>
        <span className="text-xs text-text-muted">
          {formatBytes(group.files[0].size)} &times; {group.files.length}
        </span>
        <span className="text-xs text-red-500 ml-auto">
          -{formatBytes(group.files[0].size * (group.files.length - 1))}
        </span>
      </div>
      <div className="space-y-1">
        {group.files.map((f) => {
          const isKept = f.path === keepPath;
          const fname = f.path.split("\\").pop()?.split("/").pop() || f.path;
          return (
            <div
              key={f.path}
              className={`flex items-center gap-2 rounded-md px-2 py-1.5 text-xs ${
                isKept
                  ? "bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800"
                  : "hover:bg-surface"
              }`}
            >
              <button
                onClick={() => onKeep(group.hash, f.path)}
                className={`shrink-0 w-4 h-4 rounded-full border-2 flex items-center justify-center transition-colors ${
                  isKept
                    ? "border-green-500 bg-green-500"
                    : "border-border hover:border-primary"
                }`}
              >
                {isKept && <Check size={10} className="text-white" />}
              </button>
              <span className="flex-1 truncate" title={f.path}>
                {fname}
              </span>
              <span className="text-text-muted shrink-0">
                {formatTimestamp(f.mtime)}
              </span>
              {isKept && (
                <span className="text-[10px] text-green-600 font-medium shrink-0">
                  {t("cleanup.kept")}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}