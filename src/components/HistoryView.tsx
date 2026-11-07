import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { type OrganizationLog, useAppStore } from "../store/useAppStore";
import { parentDir } from "../utils/paths";
import { buttonClass, fieldClass, primaryClass, errorText, type OperationResult, Results } from "./BetaControls";
import ConfirmDialog, { type ConfirmRequest } from "./cleanup/ConfirmDialog";

const emptyFilters = { query: "", rule: "", extension: "", from: "", to: "", run_id: "" };

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
  const [results, setResults] = useState<OperationResult[]>([]);
  const generation = useRef(0);
  const heading = useRef<HTMLHeadingElement>(null);

  const load = useCallback(async (value: typeof emptyFilters) => {
    const request = ++generation.current;
    setBusy(true);
    setError("");
    setSelected(new Set());
    try {
      if (value.from && value.to && value.from > value.to) throw new Error(t("beta.validation.date"));
      const rows = await invoke<OrganizationLog[]>("history_cmd", { filter: {
        ...value,
        from: value.from ? new Date(value.from).toISOString() : "",
        to: value.to ? new Date(value.to).toISOString() : "",
      } });
      if (request === generation.current) {
        setLogs(rows);
        setLimit(100);
      }
    } catch (cause) {
      if (request === generation.current) setError(errorText(cause));
    } finally {
      if (request === generation.current) setBusy(false);
    }
  }, [t]);
  useEffect(() => {
    void load(emptyFilters);
    return () => { ++generation.current; };
  }, [load]);

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
        const outcomes = await invoke<OperationResult[]>("undo_selected_cmd", { ids: rows.map((log) => log.id) });
        setResults(outcomes);
        await Promise.all([load(filters), useAppStore.getState().refreshActionViews()]);
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
    <form className="grid grid-cols-1 gap-3 sm:grid-cols-2" onSubmit={(event) => { event.preventDefault(); void load(filters); }}>
      {(["query", "rule", "extension", "from", "to"] as const).map((key) =>
        <label key={key} className="text-sm">{t("beta." + (key === "query" ? "search" : key))}
          <input className={fieldClass} type={key === "from" || key === "to" ? "datetime-local" : "text"}
            value={filters[key]} onChange={(event) => setFilters({ ...filters, [key]: event.target.value })} />
        </label>)}
      <div className="flex items-end gap-2">
        <button className={buttonClass} disabled={busy}>{t("beta.filter")}</button>
        <button type="button" className={buttonClass} disabled={busy} onClick={() => { setFilters(emptyFilters); void load(emptyFilters); }}>
          {t("beta.reset")}
        </button>
      </div>
    </form>
    {error && <p role="alert" className="text-red-700 dark:text-red-300">{error}</p>}
    <Results results={results} />
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
