import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type { OperationResult } from "../components/BetaControls";
import { useAppStore } from "./useAppStore";

type ApplyOutcome =
  | { status: "completed"; results: OperationResult[] }
  | { status: "failed"; error: string };
type ApplyReceipt = { status: "applying" } | (ApplyOutcome & { presented?: true });

interface ReviewQueueState {
  requestedPaths: string[] | null;
  hasUnopenedRequest: boolean;
  applyReceipt: ApplyReceipt | null;
  readReviewRequest: () => Promise<void>;
  takeReviewNavigation: () => boolean;
  requestFullQueue: () => void;
  applyReviewedPlan: (id: string, selected: string[]) => Promise<void>;
  markReceiptPresented: (receipt: ApplyReceipt) => void;
  dismissReceipt: () => void;
}

// Serialize reads from the native one-slot request across Settings mounts.
let lastRequestRead = Promise.resolve();

// The workspace WebView owns submitted work and its receipt. A mounted
// ReviewQueue owns only its unconfirmed preview and selection.
export const useReviewQueueStore = create<ReviewQueueState>((set, get) => ({
  requestedPaths: null,
  hasUnopenedRequest: false,
  applyReceipt: null,

  readReviewRequest: () => {
    const previous = get().applyReceipt;
    const read = lastRequestRead.then(async () => {
      const paths = await invoke<string[] | null>("take_review_request_cmd");
      if (!Array.isArray(paths)) return;
      const advance = previous && previous.status !== "applying" && previous.presented && get().applyReceipt === previous;
      set({ requestedPaths: paths, hasUnopenedRequest: true, ...(advance ? { applyReceipt: null } : {}) });
    });
    lastRequestRead = read.catch(() => {});
    return read;
  },

  // A native read can finish between Settings mounts.
  takeReviewNavigation: () => {
    if (!get().hasUnopenedRequest) return false;
    set({ hasUnopenedRequest: false });
    return true;
  },

  requestFullQueue: () => {
    const { applyReceipt, hasUnopenedRequest, requestedPaths } = get();
    if (!applyReceipt && !hasUnopenedRequest && requestedPaths !== null) set({ requestedPaths: null });
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

  markReceiptPresented: (receipt) => {
    if (receipt.status !== "applying" && !receipt.presented && get().applyReceipt === receipt) {
      set({ applyReceipt: { ...receipt, presented: true } });
    }
  },

  dismissReceipt: () => {
    const receipt = get().applyReceipt;
    if (receipt && receipt.status !== "applying") set({ applyReceipt: null });
  },
}));
