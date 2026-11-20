import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { type OrganizationLog, type UndoResult, useAppStore } from "../store/useAppStore";
import { parentDir } from "../utils/paths";
import { UNDO_TONE_CLASS, undoNotice } from "../utils/undo";
import { buttonClass, fieldClass, primaryClass, errorText } from "./BetaControls";
import ConfirmDialog, { type ConfirmRequest } from "./cleanup/ConfirmDialog";

const emptyFilters = { query: "", rule: "", extension: "", from: "", to: "", run_id: "" };

interface SelectedUndoResult extends UndoResult {
  id: number;
}

function canRestore(log: OrganizationLog) {
  return log.id !== undefined && !log.undone && log.undo_verified && ["move", "rename", "accept"].includes(log.action);
}

export default function HistoryView() {
  const { t } = useTranslation();
  const [filters, setFilters] = useState(emptyFilters);
  const [logs, setLogs] = useState<OrganizationLog[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [limit, setLimit] = useState(100);
  const [results, setResults] = useState<Array<SelectedUndoResult & { source: string }>>([]);
  const appliedFilters = useRef(emptyFilters);
  const generation = useRef(0);
  const heading = useRef<HTMLHeadingElement>(null);

  const load = useCallback(async (resetSelection = false) => {
    const request = ++generation.current;
    const value = appliedFilters.current;
    setBusy(true);
    setError("");
    if (resetSelection) {
      setSelected(new Set());
      setLimit(100);
    }
    try {
      const rows = await invoke<OrganizationLog[]>("history_cmd", { filter: {
        ...value,
        from: value.from ? new Date(value.from).toISOString() : "",
        to: value.to ? new Date(value.to).toISOString() : "",
      } });
      if (request === generation.current) {
        setLogs(rows);
        setSelected((previous) => new Set(rows.filter((row) => canRestore(row) && previous.has(row.id!)).map((row) => row.id!)));
      }
    } catch (cause) {
      if (request === generation.current) setError(errorText(cause));
    } finally {
      if (request === generation.current) setBusy(false);
    }
  }, []);
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    const refresh = () => { if (!disposed) void load(); };
    void listen("mutation-completed", refresh).then((unlisten) => {
      if (disposed) unlisten();
      else { stop = unlisten; refresh(); }
    }).catch((cause) => {
      console.error("History listener failed", cause);
      refresh();
    });
    return () => { disposed = true; ++generation.current; stop?.(); };
  }, [load]);

  function applyFilters(value: typeof emptyFilters) {
    if (value.from && value.to && value.from > value.to) {
      setError(t("beta.validation.date"));
      return;
    }
    appliedFilters.current = value;
    void load(true);
  }

  function toggle(id: number) {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function askRestore(opener: HTMLButtonElement) {
    const rows = logs.filter((log) => canRestore(log) && selected.has(log.id!));
    if (!rows.length) return;
    setConfirm({
      title: t("beta.confirmUndo", { count: rows.length }),
      count: rows.length,
      notice: "move",
      note: t("beta.undoHelp"),
      preview: rows.map((log) => log.source_path),
      opener,
      focusFallback: () => heading.current,
      onConfirm: async () => {
        const sources = new Map(rows.map((log) => [log.id!, log.source_path]));
        const outcomes = await invoke<SelectedUndoResult[]>("undo_selected_cmd", { ids: [...sources.keys()] });
        setResults(outcomes.map((outcome) => ({ ...outcome, source: sources.get(outcome.id) ?? "" })));
        await Promise.all([load(), useAppStore.getState().refreshActionViews()]);
      },
    });
  }

  const groups = new Map<string, OrganizationLog[]>();
  for (const log of logs.slice(0, limit)) {
    const key = log.run_id || "legacy";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(log);
  }

  return <section className="space-y-4" aria-busy={busy}>
    <h3 ref={heading} tabIndex={-1} className="text-sm font-medium">{t("beta.filter")}</h3>
    <form className="grid grid-cols-1 gap-3 sm:grid-cols-2" onSubmit={(event) => { event.preventDefault(); applyFilters(filters); }}>
      {(["query", "rule", "extension", "from", "to"] as const).map((key) =>
        <label key={key} className="text-sm">{t("beta." + (key === "query" ? "search" : key))}
          <input className={fieldClass} type={key === "from" || key === "to" ? "datetime-local" : "text"}
            value={filters[key]} onChange={(event) => setFilters({ ...filters, [key]: event.target.value })} />
        </label>)}
      <div className="flex items-end gap-2">
        <button className={buttonClass} disabled={busy}>{t("beta.filter")}</button>
        <button type="button" className={buttonClass} disabled={busy} onClick={() => { setFilters(emptyFilters); applyFilters(emptyFilters); }}>
          {t("beta.reset")}
        </button>
      </div>
    </form>
    {error && <p role="alert" className="text-red-700 dark:text-red-300">{error}</p>}
    {results.length > 0 && <div role="status" className="space-y-2 rounded-lg border border-border p-3">
      <h3 className="font-medium">{t("beta.results")}</h3>
      {results.map((result) => <div key={result.id} className="space-y-1 break-all text-sm">
        <p className="font-medium">{result.source}</p>
        {undoNotice(result).map((notice) => <p key={notice.key} className={`rounded-md px-2 py-1 ${UNDO_TONE_CLASS[notice.tone]}`}>
          {t(notice.key, notice.values)}
        </p>)}
      </div>)}
    </div>}
    <div className="flex flex-wrap items-center gap-2">
      <button className={buttonClass} disabled={busy}
        onClick={() => setSelected(new Set(logs.filter(canRestore).map((log) => log.id!)))}>{t("beta.selectAll")}</button>
      <button className={buttonClass} disabled={busy} onClick={() => setSelected(new Set())}>{t("beta.clearSelection")}</button>
      <span>{t("beta.selected", { count: selected.size })}</span>
      <button className={primaryClass} disabled={busy || !selected.size}
        onClick={(event) => askRestore(event.currentTarget)}>{t("beta.undoSelection")}</button>
    </div>
    {!busy && !logs.length && <p>{t("beta.noHistory")}</p>}
    {[...groups].map(([key, entries]) => <section key={key} className="space-y-3 rounded-lg border border-border p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 className="text-sm font-medium">
          {key === "legacy" ? t("beta.legacy") : t("beta.run") + " · " + t("beta." + entries[0].trigger, { defaultValue: entries[0].trigger })}
          <span className="block text-xs text-text-muted">{new Date(entries[0].timestamp).toLocaleString()}</span>
        </h4>
        <button className={buttonClass} disabled={busy} onClick={() => setSelected((previous) => new Set([
          ...previous,
          ...logs.filter((log) => (log.run_id || "legacy") === key && canRestore(log)).map((log) => log.id!),
        ]))}>{t("beta.selectRun")}</button>
      </div>
      {entries.map((log) => <div key={log.id} className="flex flex-wrap justify-between gap-2 border-t border-border pt-2 text-sm">
        <label className="flex min-w-0 gap-3">
          <input type="checkbox" className="mt-1 shrink-0" checked={selected.has(log.id!)}
            disabled={busy || !canRestore(log)} onChange={() => toggle(log.id!)} />
          <span className="min-w-0 break-all">
            <strong>{log.file_name}</strong> · {log.file_type} · {t("beta." + log.action, { defaultValue: log.action })}
            <span className="block">{log.source_path}</span>
            {log.destination_path && <span className="block">→ {log.destination_path}</span>}
            <span className="block text-xs text-text-muted">
              {new Date(log.timestamp).toLocaleString()} · {log.file_size} B
              {log.undone ? " · " + t("beta.undone")
                : log.action === "delete" ? " · " + t("beta.trashHelp")
                : !log.undo_verified ? " · " + t("beta.errors.history.unverified") : ""}
            </span>
          </span>
        </label>
        {log.destination_path && <button type="button" className="shrink-0 self-start text-xs text-primary"
          onClick={() => void invoke("open_folder_cmd", { path: parentDir(log.destination_path!) })
            .catch((cause) => setError(errorText(cause)))}>{t("cleanup.openFolder")}</button>}
      </div>)}
    </section>)}
    {logs.length > limit && <button className={buttonClass} onClick={() => setLimit(limit + 100)}>{t("beta.more")}</button>}
    {confirm && <ConfirmDialog {...confirm} onClose={() => setConfirm(null)} />}
  </section>;
}
