import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { OperationResult } from "../components/BetaControls";
import { useAppStore } from "./useAppStore";

type ApplyReceipt =
  | { status: "applying" }
  | { status: "completed"; results: OperationResult[] }
  | { status: "failed"; error: string };

interface ReviewQueueState {
  requestedPaths: string[] | null;
  applyReceipt: ApplyReceipt | null;
  readReviewRequest: () => Promise<void>;
  applyReviewedPlan: (id: string, selected: string[]) => Promise<void>;
  dismissReceipt: () => void;
}

// Serialize reads from the native one-slot request across Settings mounts.
let lastRequestRead = Promise.resolve();

// The workspace WebView owns submitted work and its receipt. A mounted
// ReviewQueue owns only its unconfirmed preview and selection.
export const useReviewQueueStore = create<ReviewQueueState>((set, get) => ({
  requestedPaths: null,
  applyReceipt: null,

  readReviewRequest: () => {
    const read = lastRequestRead.then(async () => {
      const paths = await invoke<string[] | null>("take_review_request_cmd");
      if (Array.isArray(paths)) set({ requestedPaths: paths });
    });
    lastRequestRead = read.catch(() => {});
    return read;
  },

  applyReviewedPlan: async (id, selected) => {
    if (get().applyReceipt) return;
    set({ applyReceipt: { status: "applying" } });
    let results: OperationResult[];
    try {
      results = await invoke<OperationResult[]>("apply_preview_cmd", { id, selected });
    } catch (error) {
      set({ applyReceipt: { status: "failed", error: String(error) } });
      return;
    }
    set({ applyReceipt: { status: "completed", results } });
    const store = useAppStore.getState();
    void Promise.allSettled([store.refreshActionViews(), store.getPendingFiles()]);
  },

  dismissReceipt: () => {
    const receipt = get().applyReceipt;
    if (receipt && receipt.status !== "applying") set({ applyReceipt: null });
  },
}));
