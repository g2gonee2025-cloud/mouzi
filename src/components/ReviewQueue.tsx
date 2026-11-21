import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { buttonClass, primaryClass, errorText, Results } from './BetaControls';
import { useReviewQueueStore } from '../store/useReviewQueueStore';
import ConfirmDialog, { type ConfirmRequest } from './cleanup/ConfirmDialog';

interface Entry { id: string; selectionKey?: string; source: string; destination: string | null; rule: string; action: string; size: number; warning: boolean; error: string | null }
interface Preview {
  id: string;
  entries: Entry[];
  unavailableRoots?: string[];
  blockedRoots?: { root: string; error: string }[];
}

function isOrdinaryProposal(entry: Entry) {
  return !entry.error && !entry.warning && entry.action !== 'delete';
}

function selectionKeys(entries: Entry[]) {
  return new Set(entries.flatMap(entry => isOrdinaryProposal(entry) && entry.selectionKey ? [entry.selectionKey] : []));
}

export default function ReviewQueue() {
  const { t } = useTranslation();
  const paths = useReviewQueueStore(state => state.requestedPaths);
  const receipt = useReviewQueueStore(state => state.applyReceipt);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [preparing, setPreparing] = useState(false);
  const [error, setError] = useState('');
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const currentId = useRef<string | null>(null);
  const submittedScope = useRef<{ paths: string[] | null } | null>(null);
  const generation = useRef(0);
  // A non-null set means the user chose the selection, even when it is empty.
  const selectionIntent = useRef<Set<string> | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const applying = receipt?.status === 'applying';
  const busy = preparing || applying;

  async function refresh() {
    const workspace = useReviewQueueStore.getState();
    if (workspace.applyReceipt?.status === 'applying' || workspace.requestedPaths !== paths) return;
    workspace.dismissReceipt();
    const request = ++generation.current;
    const current = () => request === generation.current && useReviewQueueStore.getState().requestedPaths === paths;
    const intent = selectionIntent.current;
    const previous = currentId.current;
    currentId.current = null;
    setPreparing(true); setError(''); setConfirm(null);
    setPreview(null);
    if (content.current) content.current.scrollTop = 0;
    try {
      if (previous) await invoke('discard_preview_cmd', { id: previous });
      if (!current()) return;
      const plan = await invoke<Preview>('preview_cmd', { paths: paths?.length ? paths : null });
      if (!current()) { await invoke('discard_preview_cmd', { id: plan.id }); return; }
      currentId.current = plan.id; setPreview(plan);
      const ordinary = plan.entries.filter(isOrdinaryProposal);
      const carry = intent === null ? ordinary : ordinary.filter(entry => entry.selectionKey && intent.has(entry.selectionKey));
      setSelected(new Set(carry.map(entry => entry.id)));
      if (intent !== null) selectionIntent.current = selectionKeys(carry);
    } catch (error) { if (current()) setError(errorText(error)); }
    finally { if (current()) setPreparing(false); }
  }
  useEffect(() => {
    selectionIntent.current = null;
    if (!useReviewQueueStore.getState().applyReceipt) void refresh();
    return () => {
      ++generation.current;
      const abandoned = currentId.current;
      currentId.current = null;
      if (abandoned) void invoke('discard_preview_cmd', { id: abandoned }).catch(() => {});
    };
  }, [paths]);

  // Only a receipt shown in the submitting view can yield to a fresh native request.
  useLayoutEffect(() => {
    if (!confirm && receipt && submittedScope.current?.paths === paths) {
      useReviewQueueStore.getState().markReceiptPresented(receipt);
    }
  }, [confirm, paths, receipt]);

  function askApply(opener: HTMLButtonElement) {
    if (busy || receipt || !preview) return;
    const entries = preview.entries.filter((entry) => !entry.error && selected.has(entry.id));
    if (!entries.length) return;
    const id = preview.id;
    const request = generation.current;
    setConfirm({
      title: t('beta.confirm', { count: entries.length }),
      count: entries.length,
      bytes: entries.reduce((size, entry) => size + entry.size, 0),
      notice: 'move',
      preview: entries.map(({ source, destination }) => destination === null ? source : { source, destination }),
      opener,
      focusFallback: () => heading.current,
      onConfirm: async () => {
        const workspace = useReviewQueueStore.getState();
        if (workspace.applyReceipt || workspace.requestedPaths !== paths
          || request !== generation.current || currentId.current !== id) return;
        // Transfer ownership before dispatch. Cleanup may run while the call
        // waits in the WebView transport or behind another native operation.
        currentId.current = null;
        submittedScope.current = { paths };
        if (content.current) content.current.scrollTop = 0;
        setPreview(null);
        setSelected(new Set());
        setError('');
        await workspace.applyReviewedPlan(id, entries.map(entry => entry.id));
      },
    });
  }
  const choose = (ids: Set<string>) => {
    selectionIntent.current = selectionKeys((preview?.entries ?? []).filter(entry => ids.has(entry.id)));
    setSelected(ids);
  };
  const toggle = (id: string) => {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    choose(next);
  };
  const hasOmissions = !!(preview?.unavailableRoots?.length || preview?.blockedRoots?.length);
  const hasEntries = !!preview?.entries.length;
  const visibleError = receipt?.status === 'failed' ? errorText(receipt.error) : error;
  return <section className="flex min-h-0 flex-1 flex-col gap-3" aria-busy={busy}>
    <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
      <h2 ref={heading} tabIndex={-1} className="text-lg font-semibold">{t('beta.review')}</h2>
      <button className={buttonClass} onClick={refresh} disabled={busy}>{t('beta.refresh')}</button>
    </div>
    <p className="shrink-0 text-sm text-text-muted">{t('beta.previewHelp')}</p>
    {preview && hasEntries && (
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-border pb-3">
        <div className="flex flex-wrap items-center gap-2">
          <button className={buttonClass} disabled={busy} onClick={() => choose(new Set(preview.entries.filter(isOrdinaryProposal).map(entry => entry.id)))}>{t('beta.selectAll')}</button>
          <button className={buttonClass} disabled={busy} onClick={() => choose(new Set())}>{t('beta.clearSelection')}</button>
        </div>
        <div className="ml-auto flex flex-wrap items-center justify-end gap-2">
          <span role="status" className="text-sm tabular-nums">{t('beta.selected', { count: selected.size })}</span>
          <button className={primaryClass} disabled={busy || !selected.size} onClick={(event) => askApply(event.currentTarget)}>{t('beta.apply')}</button>
        </div>
      </div>
    )}
    <div ref={content} role="region" aria-label={t('beta.review')} tabIndex={hasEntries ? 0 : undefined} className="min-h-24 space-y-3 overflow-y-auto pr-1">
      {applying && <p role="status">{t('confirm.working')}</p>}
      {preparing && <p role="status">{t('beta.loading')}</p>}
      {visibleError && <p role="alert" className="text-sm text-red-700 dark:text-red-300">{visibleError}</p>}
      <Results results={receipt?.status === 'completed' ? receipt.results : []} />
      {preview && <>
        {hasOmissions && (
          <div role="status" className="space-y-1 text-sm text-amber-800 dark:text-amber-200">
            <p>{t('beta.unavailableFolders')}</p>
            <ul className="list-disc space-y-1 pl-5">
              {preview.unavailableRoots?.map(path => <li key={path} className="break-all">{path}</li>)}
              {preview.blockedRoots?.map(({ root, error }) => <li key={root} className="break-words">
                <p>{root}</p>
                <p>{errorText(error)}</p>
              </li>)}
            </ul>
          </div>
        )}
        {!hasEntries && !hasOmissions && <p className="py-8 text-text-muted">{t('beta.empty')}</p>}
        <div className="space-y-2">{preview.entries.map(entry => <div key={entry.id} className="rounded-lg border border-border p-3">
          <label className="flex gap-3 items-start"><input type="checkbox" className="mt-1" checked={selected.has(entry.id)} disabled={busy || !!entry.error} onChange={() => toggle(entry.id)} />
            <span className="min-w-0 text-sm break-all"><span className="block font-medium">{entry.source}</span><span className="block mt-1">{entry.rule} · {t(`beta.${entry.action}`)} · {entry.size.toLocaleString()} B</span>{entry.destination && <span className="block text-text-muted">{entry.destination}</span>}</span>
          </label>
          {entry.warning && <p className="mt-2 text-sm text-amber-800 dark:text-amber-200">{t('beta.warning')}</p>}
          {entry.error && <p className="mt-2 text-sm text-text-muted">{entry.error === 'preview.recoveryUnavailable' ? t('beta.recoveryUnavailable') : errorText(entry.error)}</p>}
        </div>)}</div>
      </>}
    </div>
    {confirm && <ConfirmDialog {...confirm} onClose={() => setConfirm((active) => active === confirm ? null : active)} />}
  </section>;
}
