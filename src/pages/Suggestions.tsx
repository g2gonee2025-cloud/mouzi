import { useEffect, useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { navigateHash } from "../utils/paths";
import { useSuggestionsStore } from "../store/useSuggestionsStore";
import {
  Sparkles,
  ChevronLeft,
  Check,
  X,
  RefreshCw,
  Lightbulb,
  BrainCircuit,
  BookOpen,
  CheckSquare,
  XSquare,
} from "lucide-react";
import ConfirmDialog, { type ConfirmRequest } from "../components/cleanup/ConfirmDialog";

function confidenceColor(conf: number): string {
  if (conf >= 0.7) return "text-green-600 dark:text-green-400";
  if (conf >= 0.4) return "text-yellow-600 dark:text-yellow-400";
  return "text-gray-500 dark:text-gray-400";
}

function sourceIcon(source: string) {
  switch (source) {
    case "heuristic":
      return <Lightbulb size={14} className="text-orange-500" />;
    case "ollama":
      return <BrainCircuit size={14} className="text-purple-500" />;
    case "learned":
      return <BookOpen size={14} className="text-blue-500" />;
    default:
      return null;
  }
}

function sourceLabel(source: string, t: (key: string) => string): string {
  switch (source) {
    case "heuristic":
      return t("suggestions.source.heuristic");
    case "ollama":
      return t("suggestions.source.ollama");
    case "learned":
      return t("suggestions.source.learned");
    default:
      return source;
  }
}

export default function Suggestions() {
  const { t } = useTranslation();
  const {
    suggestions,
    loading,
    busy,
    error,
    createRuleDefault,
    results,
    loadSuggestions,
    acceptSuggestion,
    dismissSuggestion,
    acceptAll,
    dismissAll,
    clearResults,
    clearError,
  } = useSuggestionsStore();

  const [createRule, setCreateRule] = useState(createRuleDefault);
  const [pending, setPending] = useState<ConfirmRequest | null>(null);

  useEffect(() => {
    loadSuggestions();
  }, [loadSuggestions]);

  // Refresh on window focus
  const handleFocus = useCallback(() => {
    loadSuggestions();
  }, [loadSuggestions]);

  useEffect(() => {
    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [handleFocus]);

  const handleClose = () => {
    navigateHash("dashboard");
  };

  const runAcceptAll = async () => {
    await acceptAll(createRule);
  };

  // `createRule` is pre-checked, so one click used to move every file AND leave
  // behind a permanent enabled rule per file. The dialog states both outcomes
  // and lets the rule half be called out even when the box is unticked.
  const handleAcceptAllClick = () => {
    if (suggestions.length === 0) return;
    setPending({
      title: t("confirm.op.acceptAll"),
      count: suggestions.length,
      notice: "move",
      note: createRule
        ? t("confirm.note.acceptAllRules", { count: suggestions.length })
        : undefined,
      preview: suggestions.map((s) => s.path),
      onConfirm: runAcceptAll,
    });
  };

  return (
    <div className="h-full flex flex-col bg-surface text-text">
      {/* Header */}
      <header className="flex items-center justify-between px-5 py-3 border-b border-border bg-surface-dark shrink-0">
        <div className="flex items-center gap-2">
          <button
            onClick={handleClose}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={18} />
          </button>
          <Sparkles size={18} className="text-primary" />
          <h1 className="text-base font-semibold">{t("suggestions.title")}</h1>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={loadSuggestions}
            disabled={loading || busy}
            className="flex items-center gap-1 text-xs text-text-muted hover:text-text transition-colors disabled:opacity-50"
          >
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
            {t("suggestions.refresh")}
          </button>
        </div>
      </header>

      {/* Content */}
      <div className="flex-1 overflow-auto p-4 space-y-4">
        {error && (
          <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            {error}
            <button onClick={clearError} className="ml-2 underline">
              {t("suggestions.dismiss")}
            </button>
          </div>
        )}

        {/* Results banner */}
        {results && results.length > 0 && (
          <div className="rounded-lg border border-border bg-surface-dark p-4 space-y-2">
            <div className="flex items-center justify-between">
              <span className="text-sm font-semibold">
                {t("suggestions.resultsTitle")}
              </span>
              <button
                onClick={clearResults}
                className="text-xs text-text-muted hover:text-text underline"
              >
                {t("suggestions.backToList")}
              </button>
            </div>
            <div className="space-y-1 max-h-40 overflow-auto">
              {results.map((r, i) => (
                <div
                  key={i}
                  className="text-xs flex items-center gap-2"
                >
                  <span
                    className={`w-2 h-2 rounded-full shrink-0 ${
                      r.status === "ok"
                        ? "bg-green-500"
                        : r.status === "missing"
                          ? "bg-yellow-500"
                          : "bg-red-500"
                    }`}
                  />
                  <span className="truncate">{r.path}</span>
                  {r.status === "ok" && r.dest && (
                    <span className="text-text-muted truncate">
                      → {r.dest}
                    </span>
                  )}
                  {r.status === "ok" && (
                    <span className="text-green-600 dark:text-green-400 shrink-0">
                      {t("suggestions.accepted")}
                    </span>
                  )}
                  {r.status === "missing" && (
                    <span className="text-yellow-600 dark:text-yellow-400 shrink-0">
                      {t("suggestions.missing")}
                    </span>
                  )}
                  {r.status === "failed" && (
                    <span className="text-red-600 dark:text-red-400 shrink-0">
                      {t("suggestions.failed")}: {r.message}
                    </span>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Loading state */}
        {loading && suggestions.length === 0 && (
          <div className="flex items-center justify-center h-48">
            <div className="animate-pulse text-sm text-text-muted">
              {t("app.loading")}
            </div>
          </div>
        )}

        {/* Empty state */}
        {!loading && suggestions.length === 0 && !results && (
          <div className="flex flex-col items-center justify-center h-48 text-center">
            <Sparkles size={32} className="text-text-muted mb-2" />
            <p className="text-sm text-text-muted">{t("suggestions.empty")}</p>
            <p className="text-xs text-text-muted mt-1">
              {t("suggestions.hint")}
            </p>
          </div>
        )}

        {/* Suggestion list */}
        {suggestions.length > 0 && (
          <>
            {/* Bulk actions bar */}
            <div className="flex items-center justify-between flex-wrap gap-2">
              <div className="flex items-center gap-2">
                <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                  <input
                    type="checkbox"
                    checked={createRule}
                    onChange={(e) => setCreateRule(e.target.checked)}
                    className="rounded border-border"
                  />
                  {t("suggestions.createRule")}
                </label>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={handleAcceptAllClick}
                  disabled={busy}
                  className="flex items-center gap-1 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-hover transition-colors disabled:opacity-50"
                >
                  <CheckSquare size={14} />
                  {t("suggestions.acceptAll")}
                </button>
                <button
                  onClick={dismissAll}
                  disabled={busy}
                  className="flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs font-medium text-text-muted hover:text-text hover:bg-border transition-colors disabled:opacity-50"
                >
                  <XSquare size={14} />
                  {t("suggestions.dismissAll")}
                </button>
              </div>
            </div>

            {/* Suggestion cards */}
            <div className="space-y-2">              {suggestions.map((s) => (
                <div
                  key={s.path}
                  className="rounded-lg border border-border bg-surface-dark p-3 flex items-start gap-3"
                >
                  {/* Main content */}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="text-sm font-medium truncate">
                        {s.filename}
                      </span>
                      <span className="text-xs px-1.5 py-0.5 rounded bg-yellow-100 dark:bg-yellow-900/30 text-yellow-700 dark:text-yellow-400">
                        {s.currentCategory}
                      </span>
                      <span className="text-xs text-text-muted">→</span>
                      <span className="text-xs px-1.5 py-0.5 rounded bg-primary/10 text-primary font-medium">
                        {s.suggestedCategory}
                      </span>
                    </div>
                    <div className="flex items-center gap-3 mt-1.5 text-xs text-text-muted">
                      <span className="truncate max-w-64" title={s.path}>
                        {s.path}
                      </span>
                      <span
                        className={`flex items-center gap-1 font-medium ${confidenceColor(s.confidence)}`}
                      >
                        <span>
                          {Math.round(s.confidence * 100)}%
                        </span>
                        <span className="text-text-muted">
                          {t("suggestions.confidence")}
                        </span>
                      </span>
                      <span className="flex items-center gap-1">
                        {sourceIcon(s.source)}
                        {sourceLabel(s.source, t)}
                      </span>
                    </div>
                  </div>

                  {/* Actions */}
                  <div className="flex items-center gap-1 shrink-0">
                    <button
                      onClick={() => acceptSuggestion(s, createRule)}
                      disabled={busy}
                      className="p-1.5 rounded-md bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400 hover:bg-green-200 dark:hover:bg-green-900/50 transition-colors disabled:opacity-50"
                      title={t("suggestions.accept")}
                    >
                      <Check size={16} />
                    </button>
                    <button
                      onClick={() => dismissSuggestion(s.path)}
                      disabled={busy}
                      className="p-1.5 rounded-md bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400 hover:bg-red-200 dark:hover:bg-red-900/50 transition-colors disabled:opacity-50"
                      title={t("suggestions.dismiss")}
                    >
                      <X size={16} />
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      {pending && (
        <ConfirmDialog {...pending} onClose={() => setPending(null)} />
      )}
    </div>
  );
}