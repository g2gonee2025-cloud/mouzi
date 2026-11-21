import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { navigateHash } from "../utils/paths";
import { useSuggestionsStore, distinctSuggestionRules, isMovedWithWarning, type AcceptOutcome, type Suggestion } from "../store/useSuggestionsStore";
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
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import { usePreviewOwner } from "../components/cleanup/usePreviewOwner";

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

interface PendingSuggestionReview {
  dialog: ConfirmDialogProps;
  previousResults: AcceptOutcome[] | null;
}

export default function Suggestions() {
  const { t } = useTranslation();
  const {
    suggestions,
    localAiStatus,
    lookup,
    busy,
    error,
    createRuleDefault,
    results,
    loadSuggestions,
    prepareSuggestions,
    preparedPlans,
    dismissChanged,
    dismissSuggestions,
    acceptAll,
    resetDismissed,
    clearResults,
    clearError,
  } = useSuggestionsStore();
  const loading = lookup.status === "loading";
  const canAct = lookup.status === "ready" && !busy && !preparedPlans;

  const [createRule, setCreateRule] = useState(createRuleDefault);
  const [pending, setPending] = useState<PendingSuggestionReview | null>(null);
  const [preparing, setPreparing] = useState<AbortController | null>(null);
  const beginPreview = usePreviewOwner();
  const pageHeading = useRef<HTMLHeadingElement>(null);
  const resultsHeading = useRef<HTMLHeadingElement>(null);
  // A native modal makes the rest of the page inert. Let it close before
  // adding new outcomes to the already-mounted live region outside it.
  const visibleResults = pending ? pending.previousResults : results;
  const hasResults = !!visibleResults?.length;

  useEffect(() => {
    loadSuggestions();
  }, [loadSuggestions]);

  useEffect(() => {
    const handleFocus = () => {
      if (useSuggestionsStore.getState().lookup.status !== "loading") void loadSuggestions();
    };
    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [loadSuggestions]);

  const handleClose = () => {
    navigateHash("dashboard");
  };

  const reviewSuggestions = async (captured: readonly Suggestion[], opener: HTMLButtonElement) => {
    if (!captured.length || !canAct || pending) return;
    const owner = beginPreview();
    if (!owner) return;
    setPreparing(owner);
    try {
      const plans = await prepareSuggestions(captured, createRule, owner.signal);
      if (owner.signal.aborted) return;
      const rules = distinctSuggestionRules(plans);
      const ruleNotes = rules.map((rule) => {
        const operation = rule.disposition === "create" ? t("suggestions.rulePreview") : rule.disposition === "already_covered" ? t("suggestions.ruleAlreadyCovered") : t("suggestions.ruleNotApplicable");
        return `${operation}: .${rule.extension ?? "—"} → ${t(`categories.${rule.destination}`, { defaultValue: rule.destination })}. ${rule.scope}. ${rule.message ?? ""}`;
      });
      setPending({
        previousResults: useSuggestionsStore.getState().results,
        dialog: {
          title: captured.length === 1 ? t("suggestions.accept") : t("confirm.op.acceptAll"),
          count: plans.length,
          bytes: plans.reduce((total, plan) => total + plan.size, 0),
          notice: "move",
          note: ruleNotes.length ? [t("suggestions.ruleCount", { count: rules.filter((r) => r.disposition === "create").length }), ...ruleNotes].join("\n") : undefined,
          preview: plans.map((plan) => `${plan.path} → ${plan.destination}`),
          opener,
          focusFallback: () => resultsHeading.current ?? pageHeading.current,
          onConfirm: () => acceptAll(plans),
          onClose: () => { owner.abort(); setPending(null); },
        },
      });
    } catch {
      owner.abort();
    } finally {
      setPreparing(current => current === owner ? null : current);
    }
  };
  return (
    <div className="h-full flex flex-col bg-surface text-text">
      {/* Header */}
      <header className="flex items-center justify-between px-5 py-3 border-b border-border bg-surface-dark shrink-0">
        <div className="flex items-center gap-2">
          <button
            onClick={handleClose}
            aria-label={t("common.back")}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={18} />
          </button>
          <Sparkles size={18} className="text-primary" />
          <h1 ref={pageHeading} tabIndex={-1} className="text-base font-semibold">{t("suggestions.title")}</h1>
        </div>
        <div className="flex items-center gap-2">
          {busy && preparing && !preparing.signal.aborted && (
            <button
              type="button"
              onClick={() => preparing.abort()}
              className="rounded-md border border-border px-2.5 py-1 text-xs font-medium hover:bg-border"
            >
              {t("common.cancel")}
            </button>
          )}
          <button
            onClick={() => {
              clearError();
              return loadSuggestions();
            }}
            disabled={loading || busy || !!preparedPlans}
            className="flex items-center gap-1 text-xs text-text-muted hover:text-text transition-colors disabled:opacity-50"
          >
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
            {t("suggestions.refresh")}
          </button>
          <button onClick={resetDismissed} disabled={loading || busy || !!preparedPlans} className="text-xs text-text-muted hover:text-text disabled:opacity-50">
            {t("suggestions.resetDismissed")}
          </button>
        </div>
      </header>

      {/* Content */}
      <div className="flex-1 overflow-auto p-4 space-y-4">
        {lookup.status === "ready" && localAiStatus && (
          <p role="status" className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-900/20 dark:text-amber-200">
            {t(localAiStatus === "modelMissing" ? "suggestions.localAiModelMissing" : "suggestions.localAiUnreachable")}
          </p>
        )}
        {dismissChanged && (
          <p role="status" className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-900/20 dark:text-amber-200">
            {t("suggestions.dismissChanged")}
          </p>
        )}
        {lookup.status === "failed" && (
          <div role="alert" className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-800 dark:bg-red-900/20 dark:text-red-400">
            <p className="font-medium">{t("suggestions.readFailed")}</p>
            <p className="mt-1 break-words">{lookup.reason}</p>
            <button
              type="button"
              onClick={loadSuggestions}
              disabled={busy || !!preparedPlans}
              className="mt-2 rounded-md border border-current px-3 py-1 font-medium disabled:opacity-50"
            >
              {t("common.retry")}
            </button>
          </div>
        )}
        {error && (
          <div role="alert" className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            {error}
            <button onClick={clearError} className="ml-2 underline">
              {t("suggestions.dismiss")}
            </button>
          </div>
        )}

        <div className={hasResults ? "rounded-lg border border-border bg-surface-dark p-4 space-y-2" : "sr-only"}>
          {hasResults && (
            <div className="flex items-center justify-between">
              <h2 ref={resultsHeading} tabIndex={-1} className="text-sm font-semibold">
                {t("suggestions.resultsTitle")}
              </h2>
              <button
                onClick={clearResults}
                className="text-xs text-text-muted hover:text-text underline"
              >
                {t("suggestions.backToList")}
              </button>
            </div>
          )}
          <div
            role="log"
            aria-label={t("suggestions.resultsTitle")}
            aria-live="polite"
            aria-atomic="false"
            aria-relevant="additions"
            tabIndex={hasResults ? 0 : undefined}
            className="space-y-1 max-h-40 overflow-auto"
          >
            {visibleResults?.map((r, i) => {
              const labels = {
                ok: r.dest === r.path ? "suggestions.alreadyFiled" : "suggestions.accepted",
                missing: "suggestions.missing",
                failed: "suggestions.failed",
                partial: isMovedWithWarning(r) ? "suggestions.movedWithWarning" : "suggestions.moveNeedsReview",
              };
              const color = r.status === "ok" ? "text-green-600 dark:text-green-400"
                : r.status === "failed" ? "text-red-600 dark:text-red-400"
                : "text-yellow-600 dark:text-yellow-400";
              return (
                <div key={i} className="border-b border-border py-1 last:border-0 text-xs">
                  <p className="break-all">{r.path}</p>
                  {r.dest && r.dest !== r.path && (
                    <p className="text-text-muted break-all">→ {r.dest}</p>
                  )}
                  <p className={`font-medium ${color}`}>{t(labels[r.status])}</p>
                  {r.message && <p className="text-text-muted break-words">{r.message}</p>}
                </div>
              );
            })}
          </div>
        </div>

        {/* Loading state */}
        {(lookup.status === "idle" || loading) && (
          <p role="status" className={suggestions.length
            ? "text-sm text-text-muted"
            : "flex h-48 items-center justify-center text-sm text-text-muted"}>
            {t("app.loading")}
          </p>
        )}

        {/* Empty state */}
        {lookup.status === "ready" && suggestions.length === 0 && !hasResults && (
          <div className="flex flex-col items-center justify-center h-48 text-center">
            <Sparkles size={32} className="text-text-muted mb-2" />
            <p className="text-sm text-text-muted">{t("suggestions.empty")}</p>
            <p className="text-xs text-text-muted mt-1">
              {t("suggestions.hint")}
            </p>
          </div>
        )}

        {/* Suggestion list */}
        {lookup.status !== "failed" && suggestions.length > 0 && (
          <>
            {/* Bulk actions bar */}
            <div className="flex items-center justify-between flex-wrap gap-2">
              <div className="flex items-center gap-2">
                <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                  <input
                    type="checkbox"
                    checked={createRule}
                    disabled={!canAct}
                    onChange={(e) => setCreateRule(e.target.checked)}
                    className="rounded border-border"
                  />
                  {t("suggestions.createRule")}
                </label>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={(event) => void reviewSuggestions(suggestions, event.currentTarget)}
                  disabled={!canAct}
                  className="flex items-center gap-1 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-on-primary hover:bg-primary-hover transition-colors disabled:opacity-50"
                >
                  <CheckSquare size={14} />
                  {t("suggestions.acceptAll")}
                </button>
                <button
                  onClick={() => dismissSuggestions(suggestions)}
                  disabled={!canAct}
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
                        {t(`categories.${s.currentCategory}`, { defaultValue: s.currentCategory })}
                      </span>
                      <span className="text-xs text-text-muted">→</span>
                      <span className="text-xs px-1.5 py-0.5 rounded bg-primary/10 text-primary font-medium">
                        {t(`categories.${s.suggestedCategory}`, { defaultValue: s.suggestedCategory })}
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
                      onClick={(event) => void reviewSuggestions([s], event.currentTarget)}
                      disabled={!canAct}
                      aria-label={t("suggestions.accept")}
                      className="p-1.5 rounded-md bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400 hover:bg-green-200 dark:hover:bg-green-900/50 transition-colors disabled:opacity-50"
                      title={t("suggestions.accept")}
                    >
                      <Check size={16} />
                    </button>
                    <button
                      onClick={() => dismissSuggestions([s])}
                      disabled={!canAct}
                      aria-label={t("suggestions.dismiss")}
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

      {pending && <ConfirmDialog {...pending.dialog} />}
    </div>
  );
}
