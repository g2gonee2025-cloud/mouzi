export function createPreviewSlot() {
  let detach: (() => void) | undefined;

  function clear() {
    detach?.();
    detach = undefined;
  }

  return {
    clear,
    begin(owner: AbortSignal | undefined, onCancel: () => void) {
      owner?.throwIfAborted();
      const isCurrent = () => detach === unsubscribe;
      const abort = () => {
        if (!isCurrent()) return;
        clear();
        onCancel();
      };
      const unsubscribe = () => owner?.removeEventListener("abort", abort);
      detach = unsubscribe;
      owner?.addEventListener("abort", abort, { once: true });
      return isCurrent;
    },
  };
}
