import { useId, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";
import { formatBytes } from "../../utils/format";

/**
 * Friction is chosen from what is at stake, not from which button was clicked.
 * A blanket confirm on every destructive action trains people to dismiss it
 * without reading, and then the one prompt that actually mattered - trashing
 * 40 GB - reads exactly like the one for a 2 KB stray file. Scaling the cost of
 * being wrong keeps the cheap prompts cheap and the expensive ones expensive.
 */
export type ConfirmTier = "simple" | "explicit" | "typed";

/**
 * Which reversibility promise the backend actually keeps. These are not
 * interchangeable, and stating the wrong one is worse than stating none:
 *
 * - "trash"     -> Windows Shell recycling. The outcome is recoverable only
 *                  when Mouzi can attribute the Recycle Bin entry to the
 *                  reviewed file.
 * - "permanent" -> `fs::remove_dir` for empty folders, and the plain DELETEs
 *                  behind undo / clear-history. No bin, no recovery at all.
 * - "move"      -> rules MOVE files, so nothing is removed; the only way back
 *                  is the app's own Undo, and only while the log survives.
 */
export type ConfirmNotice = "trash" | "permanent" | "move";

const EXPLICIT_MIN_ITEMS = 10;
const EXPLICIT_MIN_BYTES = 100 * 1024 * 1024;
const TYPED_MIN_ITEMS = 25;
const TYPED_MIN_BYTES = 1024 * 1024 * 1024;
const PREVIEW_LIMIT = 8;

const NOTICE_KEY = {
  trash: "confirm.trashNotice",
  move: "confirm.moveNotice",
  permanent: "confirm.permanentNotice",
} as const;

/** The one place the tiers are decided, so no two call sites can disagree. */
export function confirmTier(count: number, bytes?: number): ConfirmTier {
  const size = bytes ?? 0;
  if (count > TYPED_MIN_ITEMS || size >= TYPED_MIN_BYTES) return "typed";
  if (count >= EXPLICIT_MIN_ITEMS || size >= EXPLICIT_MIN_BYTES) return "explicit";
  return "simple";
}

export interface ConfirmRequest {
  /** Operation name. Also the confirm button's label, so the button never says
   *  "OK" about something the heading did not name. */
  title: string;
  /** Items at stake. Drives the tier. */
  count: number;
  /**
   * False when `count` is a lower bound rather than the real total - "Clean
   * Now" can only see files the watcher already queued, and the scan that runs
   * after confirmation can still find more. Also stops 0 from meaning "nothing
   * selected" and disabling the button for an action that is not a no-op.
   */
  countIsExact?: boolean;
  /** Total bytes at stake. Omit when the backend cannot know it before the run. */
  bytes?: number;
  notice: ConfirmNotice;
  /** Operation-specific consequence, already translated by the caller. */
  note?: string;
  /** Paths shown so the user sees what they are agreeing to. */
  preview?: readonly string[];
  opener?: HTMLElement;
  focusFallback?: () => HTMLElement | null;
  onConfirm: () => void | Promise<void>;
}

export interface ConfirmDialogProps extends ConfirmRequest {
  onClose: () => void;
}

export default function ConfirmDialog({
  title,
  count,
  countIsExact = true,
  bytes,
  notice,
  note,
  preview,
  opener,
  focusFallback,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  const { t } = useTranslation();
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const running = useRef(false);
  const confirmed = useRef(false);
  const pressedBackdrop = useRef(false);
  const titleId = useId();
  const bodyId = useId();
  const inputId = useId();

  const tier = confirmTier(count, bytes);
  const word = t("confirm.word");
  const nothingSelected = count <= 0 && countIsExact;
  // Compare case- and whitespace-insensitively so "  delete  " still counts,
  // while the word itself stays a real word in the prompt rather than a hash.
  const wordMatches =
    typed.trim().toLocaleLowerCase() === word.trim().toLocaleLowerCase();
  const confirmDisabled = busy || nothingSelected || (tier === "typed" && !wordMatches);

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const document = dialog.ownerDocument;
    const previous = opener ?? document.activeElement;
    dialog.showModal();
    (tier === "typed" ? inputRef.current : panelRef.current)?.focus();

    return () => {
      dialog.close();
      // Results and newly enabled controls are available after React commits.
      queueMicrotask(() => {
        if (dialog.open || document.querySelector("dialog:modal")) return;
        const fallback = focusFallback?.();
        const targets = confirmed.current ? [fallback, previous] : [previous, fallback];
        for (const target of targets) {
          if (!(target instanceof HTMLElement) || !target.isConnected || target.matches(":disabled")) continue;
          target.focus();
          if (document.activeElement === target) break;
        }
      });
    };
  }, [focusFallback, opener, tier]);

  useLayoutEffect(() => {
    if (busy) panelRef.current?.focus();
  }, [busy]);

  const dismiss = () => {
    if (!running.current) onClose();
  };

  const handleConfirm = async () => {
    if (confirmDisabled || running.current) return;
    running.current = true;
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      confirmed.current = true;
      onClose();
    } catch (e) {
      // Surfaced here on purpose: these calls used to be fired off from a click
      // handler that ignored the rejection, so a failed destructive action left
      // the user with no idea whether it had happened.
      setError(String(e));
      running.current = false;
      setBusy(false);
    }
  };

  const shownPreview = preview?.slice(0, PREVIEW_LIMIT) ?? [];
  const hiddenCount = (preview?.length ?? 0) - shownPreview.length;

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      aria-describedby={bodyId}
      className="fixed inset-0 m-0 h-full max-h-none w-full max-w-none items-center justify-center bg-transparent p-4 text-text open:flex backdrop:bg-black/50"
      onCancel={(e) => {
        e.preventDefault();
        dismiss();
      }}
      onPointerDown={(e) => { pressedBackdrop.current = e.target === e.currentTarget; }}
      onClick={(e) => {
        if (pressedBackdrop.current && e.target === e.currentTarget) dismiss();
        pressedBackdrop.current = false;
      }}
      onKeyDown={(e) => {
        if (e.key !== "Tab") return;
        const controls = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)"));
        const first = controls[0];
        const last = controls[controls.length - 1];
        const active = e.currentTarget.ownerDocument.activeElement;
        if (!controls.some((control) => control === active) || active === (e.shiftKey ? first : last)) {
          e.preventDefault();
          ((e.shiftKey ? last : first) ?? panelRef.current)?.focus();
        }
      }}
    >
      <div
        ref={panelRef}
        tabIndex={-1}
        className="max-h-full w-full max-w-md space-y-3 overflow-y-auto rounded-lg border border-border bg-surface p-4 outline-none"
      >
        <div className="flex items-start gap-2">
          {tier !== "simple" && (
            <AlertTriangle size={16} className="text-red-500 shrink-0 mt-0.5" />
          )}
          <h2 id={titleId} className="text-sm font-semibold text-text">
            {title}
          </h2>
        </div>

        <div id={bodyId} className="space-y-2">
          {tier !== "simple" && (count > 0 || countIsExact) && (
            <div className="rounded-md border border-border bg-surface-dark px-3 py-2">
              <div className="text-xs font-medium text-text tabular-nums">
                {countIsExact
                  ? t("confirm.impactItems", { count })
                  : t("confirm.impactAtLeast", { count })}
              </div>
              {bytes !== undefined && (
                <div className="text-xs text-text-muted tabular-nums">
                  {t("confirm.impactBytes", { size: formatBytes(bytes) })}
                </div>
              )}
            </div>
          )}

          {bytes === undefined && (
            <p className="text-xs text-text-muted">{t("confirm.bytesUnknown")}</p>
          )}

          {note && <p className="text-xs text-text-muted whitespace-pre-line">{note}</p>}

          {shownPreview.length > 0 && (
            <ul className="rounded-md border border-border bg-surface-dark max-h-28 overflow-auto">
              {shownPreview.map((path) => (
                <li
                  key={path}
                  className="px-2.5 py-1 text-[10px] text-text-muted truncate border-b border-border last:border-b-0"
                  title={path}
                >
                  {path}
                </li>
              ))}
            </ul>
          )}
          {hiddenCount > 0 && (
            <p className="text-[10px] text-text-muted">
              {t("confirm.more", { count: hiddenCount })}
            </p>
          )}

          <p className="text-xs text-text-muted">{t(NOTICE_KEY[notice])}</p>

          {tier === "typed" && (
            <div className="space-y-1">
              <label htmlFor={inputId} className="block text-xs text-text-muted">
                {t("confirm.typedPrompt", { word })}
              </label>
              <input
                id={inputId}
                ref={inputRef}
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.repeat && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    void handleConfirm();
                  }
                }}
                readOnly={busy}
                autoComplete="off"
                spellCheck={false}
                className="w-full rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
              />
              {typed.length > 0 && !wordMatches && (
                <p className="text-xs text-red-500">{t("confirm.typedMismatch")}</p>
              )}
            </div>
          )}

          {error && (
            <p role="alert" className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
              {t("confirm.failed", { error })}
            </p>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            type="button"
            onClick={dismiss}
            disabled={busy}
            className="flex items-center gap-1.5 rounded-md border border-border px-3 py-2 text-sm hover:bg-surface-dark transition-colors disabled:opacity-50"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={() => void handleConfirm()}
            disabled={confirmDisabled}
            className="flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-2 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-50 transition-colors"
          >
            {busy ? t("confirm.working") : title}
          </button>
        </div>
      </div>
    </dialog>
  );
}
