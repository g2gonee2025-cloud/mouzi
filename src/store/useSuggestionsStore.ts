import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { createPreviewSlot } from "./previewRequest";

export interface Suggestion {
  path: string;
  filename: string;
  suggestedCategory: string;
  confidence: number;
  source: "heuristic" | "ollama" | "learned";
  currentCategory: string;
  version: {
    identity: string;
    size: string;
    mtimeNs: string;
  };
}

type LocalAiStatus = "modelMissing" | "unreachable" | "generationFailed" | null;
interface SuggestionPolicyIssue {
  root: string;
  message: string;
}

export interface SuggestionsResponse {
  suggestions: Suggestion[];
  localAiStatus: LocalAiStatus;
  policyIssues?: SuggestionPolicyIssue[];
}

export interface AcceptOutcome {
  path: string;
  status: "ok" | "missing" | "failed" | "partial";
  message: string | null;
  dest: string | null;
}

export function isMovedWithWarning(outcome: AcceptOutcome): boolean {
  return outcome.status === "partial" && !!outcome.dest && outcome.dest !== outcome.path;
}

export interface SuggestionRulePreview {
  extension: string | null;
  destination: string;
  scope: string;
  priority: number;
  disposition: "create" | "already_covered" | "conflict" | "not_applicable";
  message: string | null;
}
export interface SuggestionPlan {
  planId: string;
  path: string;
  category: string;
  destination: string;
  size: number;
  rule: SuggestionRulePreview | null;
}
export function distinctSuggestionRules(plans: readonly SuggestionPlan[]): SuggestionRulePreview[] {
  const distinct = new Map<string, SuggestionRulePreview>();
  for (const { rule } of plans) {
    if (rule) distinct.set(JSON.stringify([rule.extension, rule.destination, rule.scope, rule.priority, rule.disposition]), rule);
  }
  return [...distinct.values()];
}

type SuggestionsLookup =
  | { status: "idle" | "loading" | "ready" }
  | { status: "failed"; reason: string };

interface SuggestionsState {
  suggestions: Suggestion[];
  localAiStatus: LocalAiStatus;
  policyIssues: SuggestionPolicyIssue[];
  lookup: SuggestionsLookup;
  busy: boolean;
  error: string | null;
  createRuleDefault: boolean;
  preparedPlans: readonly SuggestionPlan[] | null;
  results: AcceptOutcome[] | null;
  loadSuggestions: () => Promise<void>;
  prepareSuggestions: (suggestions: readonly Suggestion[], createRule: boolean, owner?: AbortSignal) => Promise<readonly SuggestionPlan[]>;
  acceptAll: (plans: readonly SuggestionPlan[]) => Promise<void>;
  dismissChanged: boolean;
  dismissSuggestions: (suggestions: readonly Suggestion[]) => Promise<void>;
  resetDismissed: () => Promise<void>;
  clearResults: () => void;
  clearError: () => void;
}

let loadGeneration = 0;
const preview = createPreviewSlot();

function isCurrentSelection(state: SuggestionsState, captured: readonly Suggestion[]): boolean {
  return state.lookup.status === "ready" && captured.length > 0
    && captured.every((item) => state.suggestions.includes(item));
}

async function refreshSuggestions(keepDismissalNotice = false): Promise<void> {
  const { getState, setState } = useSuggestionsStore;
  const { busy, preparedPlans, dismissChanged } = getState();
  if (busy || preparedPlans) return;
  const generation = ++loadGeneration;
  setState({
    lookup: { status: "loading" }, localAiStatus: null, policyIssues: [],
    dismissChanged: keepDismissalNotice && dismissChanged,
  });
  try {
    const { suggestions, localAiStatus, policyIssues = [] } =
      await invoke<SuggestionsResponse>("get_suggestions_with_status_cmd", { limit: 100 });
    if (generation === loadGeneration) setState({ suggestions, localAiStatus, policyIssues, lookup: { status: "ready" } });
  } catch (error) {
    if (generation === loadGeneration) setState({ lookup: { status: "failed", reason: String(error) } });
  }
}

export const useSuggestionsStore = create<SuggestionsState>((set, get) => ({
  suggestions: [], localAiStatus: null, policyIssues: [], lookup: { status: "idle" }, busy: false, error: null,
  createRuleDefault: false, preparedPlans: null, results: null, dismissChanged: false,
  loadSuggestions: () => refreshSuggestions(),
  prepareSuggestions: async (suggestions, createRule, owner) => {
    const state = get();
    if (state.busy || state.preparedPlans) throw new Error("Another suggestion is being reviewed or processed");
    if (!isCurrentSelection(state, suggestions)) throw new Error("Suggestions changed; refresh and review again");
    let preparationId: string | undefined;
    const cancel = () => {
      if (!preparationId) return;
      const cancelled = preparationId;
      preparationId = undefined;
      void invoke("cancel_suggestion_preparation_cmd", { preparationId: cancelled })
        .catch((error) => console.error("Could not cancel suggestion preview", error));
    };
    const isCurrent = preview.begin(owner, () => {
      cancel();
      set({ busy: false, preparedPlans: null });
    });
    ++loadGeneration;
    const requests = suggestions.map(({ path, suggestedCategory }) => ({ path, suggestedCategory }));
    set({ busy: true, error: null, dismissChanged: false });
    try {
      preparationId = await invoke<string>("begin_suggestion_preparation_cmd");
      owner?.throwIfAborted();
      const plans = await invoke<SuggestionPlan[]>("prepare_suggestions_cmd", { preparationId, requests, createRule });
      owner?.throwIfAborted();
      const frozen = Object.freeze(plans.map((plan) => Object.freeze({
        ...plan,
        rule: plan.rule ? Object.freeze({ ...plan.rule }) : null,
      })));
      set({ preparedPlans: frozen, busy: false });
      return frozen;
    } catch (error) {
      cancel();
      if (isCurrent()) {
        preview.clear();
        set({ error: String(error), busy: false });
      }
      owner?.throwIfAborted();
      throw error;
    }
  },
  acceptAll: async (plans) => {
    if (get().busy || get().preparedPlans !== plans) throw new Error("Suggestion preview changed; review again");
    preview.clear();
    ++loadGeneration;
    set({ busy: true, preparedPlans: null, error: null, dismissChanged: false });
    try {
      for (const plan of plans) {
        let outcome: AcceptOutcome;
        try { outcome = await invoke<AcceptOutcome>("accept_suggestion_cmd", { planId: plan.planId }); }
        catch (error) { outcome = { path: plan.path, status: "failed", message: String(error), dest: null }; }
        const finished = outcome.status === "ok" || outcome.status === "missing" || isMovedWithWarning(outcome);
        set((state) => ({
          suggestions: finished ? state.suggestions.filter((s) => s.path !== plan.path) : state.suggestions,
          results: (state.results ?? []).concat(outcome),
        }));
      }
    } finally { set({ busy: false }); }
  },
  dismissSuggestions: async (captured) => {
    const state = get();
    if (state.busy || state.preparedPlans || !isCurrentSelection(state, captured)) return;
    ++loadGeneration;
    set({ busy: true, error: null, dismissChanged: false });
    let changed = false;
    try {
      for (const { path, version } of captured) {
        const dismissed = await invoke<boolean>("dismiss_suggestion_cmd", { path, version });
        changed ||= !dismissed;
        set((state) => ({
          suggestions: state.suggestions.filter((item) => item.path !== path
            || item.version.identity !== version.identity
            || item.version.size !== version.size
            || item.version.mtimeNs !== version.mtimeNs),
        }));
      }
    } catch (error) { set({ error: String(error) }); }
    finally { set({ busy: false, dismissChanged: changed }); }
    if (changed && !get().error) await refreshSuggestions(true);
  },
  resetDismissed: async () => {
    const { lookup, busy, preparedPlans } = get();
    if (lookup.status === "loading" || busy || preparedPlans) return;
    ++loadGeneration;
    set({ busy: true, error: null });
    try {
      await invoke("reset_dismissed_suggestions_cmd");
      set({ busy: false, dismissChanged: false });
      await get().loadSuggestions();
    } catch (error) { set({ error: String(error) }); }
    finally { set({ busy: false }); }
  },
  clearResults: () => set({ results: null }),
  clearError: () => set({ error: null }),
}));
