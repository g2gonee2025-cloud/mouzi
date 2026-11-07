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
}
export interface AcceptOutcome {
  path: string;
  status: "ok" | "missing" | "failed" | "partial";
  message: string | null;
  dest: string | null;
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
  rule: SuggestionRulePreview | null;
}
export function distinctSuggestionRules(plans: readonly SuggestionPlan[]): SuggestionRulePreview[] {
  const distinct = new Map<string, SuggestionRulePreview>();
  for (const { rule } of plans) {
    if (rule) distinct.set(JSON.stringify([rule.extension, rule.destination, rule.scope, rule.priority, rule.disposition]), rule);
  }
  return [...distinct.values()];
}

interface SuggestionsState {
  suggestions: Suggestion[];
  loading: boolean;
  busy: boolean;
  error: string | null;
  createRuleDefault: boolean;
  preparedPlans: readonly SuggestionPlan[] | null;
  results: AcceptOutcome[] | null;
  loadSuggestions: () => Promise<void>;
  prepareSuggestions: (suggestions: readonly Suggestion[], createRule: boolean, owner?: AbortSignal) => Promise<readonly SuggestionPlan[]>;
  acceptAll: (plans: readonly SuggestionPlan[]) => Promise<void>;
  dismissSuggestion: (path: string) => Promise<void>;
  dismissAll: () => Promise<void>;
  resetDismissed: () => Promise<void>;
  clearResults: () => void;
  clearError: () => void;
}

let loadGeneration = 0;
const preview = createPreviewSlot();
export const useSuggestionsStore = create<SuggestionsState>((set, get) => ({
  suggestions: [], loading: false, busy: false, error: null,
  createRuleDefault: false, preparedPlans: null, results: null,
  loadSuggestions: async () => {
    if (get().busy || get().preparedPlans) return;
    const generation = ++loadGeneration;
    set({ loading: true, error: null });
    try {
      const suggestions = await invoke<Suggestion[]>("get_suggestions_cmd", { limit: 100 });
      if (generation === loadGeneration) set({ suggestions, loading: false });
    } catch (error) {
      if (generation === loadGeneration) set({ error: String(error), loading: false });
    }
  },
  prepareSuggestions: async (suggestions, createRule, owner) => {
    if (!suggestions.length || get().busy || get().preparedPlans) throw new Error("Another suggestion is being reviewed or processed");
    const isCurrent = preview.begin(owner, () => set({ busy: false, preparedPlans: null }));
    ++loadGeneration;
    const captured = suggestions.map((s) => ({ ...s }));
    set({ busy: true, loading: false, error: null });
    try {
      const plans: SuggestionPlan[] = [];
      for (const s of captured) {
        const plan = await invoke<SuggestionPlan>("prepare_suggestion_cmd", { path: s.path, suggestedCategory: s.suggestedCategory, createRule });
        owner?.throwIfAborted();
        plans.push(Object.freeze({ ...plan, rule: plan.rule ? Object.freeze({ ...plan.rule }) : null }));
      }
      const plannedExtensions = new Map<string, string>();
      for (const { rule } of plans) {
        if (rule?.disposition !== "create" || !rule.extension) continue;
        const previous = plannedExtensions.get(rule.extension);
        if (previous && previous !== rule.destination) throw new Error(`Conflicting future rules for .${rule.extension}; accept without creating rules or review files individually`);
        plannedExtensions.set(rule.extension, rule.destination);
      }
      const frozen = Object.freeze(plans);
      set({ preparedPlans: frozen, busy: false });
      return frozen;
    } catch (error) {
      if (isCurrent()) {
        preview.clear();
        set({ error: String(error), busy: false });
      }
      throw error;
    }
  },
  acceptAll: async (plans) => {
    if (get().busy || get().preparedPlans !== plans) throw new Error("Suggestion preview changed; review again");
    preview.clear();
    ++loadGeneration;
    set({ busy: true, loading: false, preparedPlans: null, error: null });
    const errors: string[] = [];
    try {
      for (const plan of plans) {
        let outcome: AcceptOutcome;
        try { outcome = await invoke<AcceptOutcome>("accept_suggestion_cmd", { planId: plan.planId }); }
        catch (error) { outcome = { path: plan.path, status: "failed", message: String(error), dest: null }; }
        if (outcome.status === "failed" || outcome.status === "partial") errors.push(outcome.message ?? outcome.status);
        const finished = outcome.status === "ok" || outcome.status === "missing" || (outcome.status === "partial" && outcome.dest !== null);
        set((state) => ({
          suggestions: finished ? state.suggestions.filter((s) => s.path !== plan.path) : state.suggestions,
          results: (state.results ?? []).concat(outcome),
        }));
      }
    } finally { set({ busy: false, error: errors.length ? errors.join("\n") : null }); }
  },
  dismissSuggestion: async (path) => {
    if (get().busy || get().preparedPlans) return;
    ++loadGeneration;
    set({ busy: true, loading: false, error: null });
    try {
      await invoke("dismiss_suggestion_cmd", { path });
      set((state) => ({ suggestions: state.suggestions.filter((s) => s.path !== path) }));
    } catch (error) { set({ error: String(error) }); }
    finally { set({ busy: false }); }
  },
  dismissAll: async () => {
    if (get().busy || get().preparedPlans) return;
    const captured = get().suggestions.map((s) => s.path);
    ++loadGeneration;
    set({ busy: true, loading: false, error: null });
    try {
      for (const path of captured) {
        await invoke("dismiss_suggestion_cmd", { path });
        set((state) => ({ suggestions: state.suggestions.filter((s) => s.path !== path) }));
      }
    } catch (error) { set({ error: String(error) }); }
    finally { set({ busy: false }); }
  },
  resetDismissed: async () => {
    if (get().busy || get().preparedPlans) return;
    ++loadGeneration;
    set({ busy: true, loading: false, error: null });
    try {
      await invoke("reset_dismissed_suggestions_cmd");
      set({ busy: false });
      await get().loadSuggestions();
    } catch (error) { set({ error: String(error) }); }
    finally { set({ busy: false }); }
  },
  clearResults: () => set({ results: null }),
  clearError: () => set({ error: null }),
}));
