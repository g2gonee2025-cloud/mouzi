import { useLayoutEffect, useRef, useState } from "react";
import { useCleanupStore } from "../../store/useCleanupStore";
import type { CleanupRequest } from "../../utils/cleanup";
import type { ConfirmDialogProps } from "./ConfirmDialog";
import { usePreviewOwner } from "./usePreviewOwner";

type ConfirmationContext = Pick<ConfirmDialogProps, "title" | "notice" | "opener" | "focusFallback">;

interface ActiveConfirmation {
  planId: string;
  dialog: ConfirmDialogProps;
  retire: () => void;
}

export function useCleanupConfirmation() {
  const beginPreview = usePreviewOwner();
  const preparedPlan = useCleanupStore((state) => state.preparedPlan);
  const [active, setActive] = useState<ActiveConfirmation | null>(null);
  const [reviewNeedsSearch, setReviewNeedsSearch] = useState(false);
  const dispatched = useRef<string | null>(null);
  // Execution consumes the plan before it returns a receipt.
  const invalidated = active !== null && active.planId !== preparedPlan?.planId && active.planId !== dispatched.current;

  useLayoutEffect(() => {
    if (invalidated) active.retire();
  }, [invalidated, active]);

  async function openReview(actions: CleanupRequest[], context: ConfirmationContext) {
    const owner = beginPreview();
    if (!owner) return;
    try {
      const plan = await useCleanupStore.getState().prepareCleanup(actions, owner.signal);
      if (owner.signal.aborted) return;
      const close = () => {
        setActive((current) => current?.planId === plan.planId ? null : current);
        owner.abort();
      };
      const retire = () => {
        if (owner.signal.aborted) return;
        close();
        setReviewNeedsSearch(true);
      };
      setActive({
        planId: plan.planId,
        retire,
        dialog: {
          ...context,
          count: plan.actions.length,
          bytes: context.notice === "trash" ? plan.totalBytes : undefined,
          preview: plan.actions.map((action) => action.path),
          onConfirm: async () => {
            const store = useCleanupStore.getState();
            if (owner.signal.aborted) return;
            if (store.preparedPlan?.planId !== plan.planId) {
              retire();
              return;
            }
            dispatched.current = plan.planId;
            await store.executeCleanup(plan.planId);
          },
          onClose: close,
        },
      });
      setReviewNeedsSearch(false);
    } catch {
      owner.abort();
    }
  }

  return {
    pending: invalidated ? null : active?.dialog,
    reviewNeedsSearch: invalidated || reviewNeedsSearch,
    clearReviewNotice: () => setReviewNeedsSearch(false),
    openReview,
  };
}
