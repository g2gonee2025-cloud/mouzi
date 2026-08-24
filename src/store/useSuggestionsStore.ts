import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

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
  status: "ok" | "missing" | "failed";
  message: string | null;
  dest: string | null;
}

interface SuggestionsState {
  suggestions: Suggestion[];
  loading: boolean;
  busy: boolean;
  error: string | null;
  createRuleDefault: boolean;
  results: AcceptOutcome[] | null;

  loadSuggestions: () => Promise<void>;
  applyAccept: (s: Suggestion, createRule: boolean) => Promise<void>;
  acceptSuggestion: (s: Suggestion, createRule: boolean) => Promise<void>;
  dismissSuggestion: (path: string) => Promise<void>;
  acceptAll: (createRule: boolean) => Promise<void>;
  dismissAll: () => Promise<void>;
  clearResults: () => void;
  clearError: () => void;
}

export const useSuggestionsStore = create<SuggestionsState>((set, get) => ({
  suggestions: [],
  loading: false,
  busy: false,
  error: null,
  createRuleDefault: true,
  results: null,

  loadSuggestions: async () => {
    set({ loading: true, error: null, results: null });
    try {
      const suggestions = await invoke<Suggestion[]>("get_suggestions_cmd", {
        limit: 100,
      });
      set({ suggestions, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  // Internal: apply a single accept without touching busy flag.
  // acceptSuggestion and acceptAll both call this.
  applyAccept: async (s: Suggestion, createRule: boolean) => {
    const outcome = await invoke<AcceptOutcome>("accept_suggestion_cmd", {
      path: s.path,
      suggestedCategory: s.suggestedCategory,
      createRule,
    });
    set((state) => ({
      suggestions: state.suggestions.filter((x) => x.path !== s.path),
      results: (state.results ?? []).concat([outcome]),
    }));
  },

  acceptSuggestion: async (s: Suggestion, createRule: boolean) => {
    set({ busy: true, error: null });
    try {
      await get().applyAccept(s, createRule);
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ busy: false });
    }
  },

  dismissSuggestion: async (path: string) => {
    set({ busy: true, error: null });
    try {
      await invoke("dismiss_suggestion_cmd", { path });
      set((state) => ({
        suggestions: state.suggestions.filter((x) => x.path !== path),
      }));
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ busy: false });
    }
  },

  acceptAll: async (createRule: boolean) => {
    const { suggestions } = get();
    if (suggestions.length === 0) return;
    set({ busy: true, error: null });
    for (const s of suggestions) {
      try {
        await get().applyAccept(s, createRule);
      } catch (e) {
        set({ error: String(e) });
        break;
      }
    }
    set({ busy: false });
  },

  dismissAll: async () => {
    const { suggestions } = get();
    if (suggestions.length === 0) return;
    set({ busy: true });
    for (const s of suggestions) {
      try {
        await invoke("dismiss_suggestion_cmd", { path: s.path });
        set((state) => ({
          suggestions: state.suggestions.filter((x) => x.path !== s.path),
        }));
      } catch (e) {
        set({ error: String(e) });
        break;
      }
    }
    set({ busy: false });
  },

  clearResults: () => set({ results: null }),
  clearError: () => set({ error: null }),
}));