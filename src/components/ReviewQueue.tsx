import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { buttonClass, primaryClass, errorText, Results, OperationResult } from './BetaControls';
import { useAppStore } from '../store/useAppStore';
import ConfirmDialog, { type ConfirmRequest } from './cleanup/ConfirmDialog';

interface Entry { id: string; source: string; destination: string | null; rule: string; action: string; size: number; warning: boolean; error: string | null }
interface Preview { id: string; entries: Entry[] }
export default function ReviewQueue({ paths }: { paths: string[] | null }) {
  const { t } = useTranslation();
  const [preview, setPreview] = useState<Preview | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [results, setResults] = useState<OperationResult[]>([]);
  const currentId = useRef<string | null>(null);
  const generation = useRef(0);
  const heading = useRef<HTMLHeadingElement>(null);
  async function refresh() {
    const request = ++generation.current;
    const previous = currentId.current;
    currentId.current = null;
    setBusy(true); setError(''); setConfirm(null); setResults([]);
    setPreview(null);
    try {
      if (previous) await invoke('discard_preview_cmd', { id: previous });
      if (request !== generation.current) return;
      const plan = await invoke<Preview>('preview_cmd', { paths: paths?.length ? paths : null });
      if (request !== generation.current) { await invoke('discard_preview_cmd', { id: plan.id }); return; }
      currentId.current = plan.id; setPreview(plan);
      setSelected(new Set(plan.entries.filter(e => !e.error && !e.warning && e.action !== 'delete').map(e => e.id)));
    } catch (error) { if (request === generation.current) setError(errorText(error)); }
    finally { if (request === generation.current) setBusy(false); }
  }
  useEffect(() => {
    void refresh();
    return () => {
      ++generation.current;
      if (currentId.current) void invoke('discard_preview_cmd', { id: currentId.current }).catch(() => {});
    };
  }, [paths]);
  function askApply(opener: HTMLButtonElement) {
    if (!preview) return;
    const entries = preview.entries.filter((entry) => !entry.error && selected.has(entry.id));
    if (!entries.length) return;
    const id = preview.id;
    const request = generation.current;
    setConfirm({
      title: t('beta.confirm', { count: entries.length }),
      count: entries.length,
      bytes: entries.reduce((size, entry) => size + entry.size, 0),
      notice: 'move',
      preview: entries.map((entry) => entry.source),
      opener,
      focusFallback: () => heading.current,
      onConfirm: async () => {
        setBusy(true);
        try {
          const outcomes = await invoke<OperationResult[]>('apply_preview_cmd', { id, selected: entries.map((entry) => entry.id) });
          if (request === generation.current) setResults(outcomes);
          const store = useAppStore.getState();
          await Promise.allSettled([store.refreshActionViews(), store.getPendingFiles()]);
        } catch (cause) {
          if (request === generation.current) setError(errorText(cause));
        } finally {
          if (request === generation.current) {
            currentId.current = null;
            setPreview(null);
            setSelected(new Set());
            setBusy(false);
          }
        }
      },
    });
  }
  const toggle = (id: string) => setSelected(old => { const next = new Set(old); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  return <section className="space-y-4" aria-busy={busy}>
    <div className="flex flex-wrap justify-between items-center gap-3"><h2 ref={heading} tabIndex={-1} className="text-lg font-semibold">{t('beta.review')}</h2><button className={buttonClass} onClick={refresh} disabled={busy}>{t('beta.refresh')}</button></div>
    <p className="text-sm text-text-muted">{t('beta.previewHelp')}</p>
    {busy && <p role="status">{t('beta.loading')}</p>}
    {error && <p role="alert" className="text-sm text-red-700 dark:text-red-300">{error}</p>}
    <Results results={results} />
    {preview && <>
      <div className="flex flex-wrap items-center gap-2">
        <button className={buttonClass} disabled={busy} onClick={() => setSelected(new Set(preview.entries.filter(e => !e.error && !e.warning && e.action !== 'delete').map(e => e.id)))}>{t('beta.selectAll')}</button>
        <button className={buttonClass} disabled={busy} onClick={() => setSelected(new Set())}>{t('beta.clearSelection')}</button>
        <span className="text-sm">{t('beta.selected', { count: selected.size })}</span>
      </div>
      {!preview.entries.length && <p className="py-8 text-text-muted">{t('beta.empty')}</p>}
      <div className="space-y-2">{preview.entries.map(entry => <div key={entry.id} className="rounded-lg border border-border p-3">
        <label className="flex gap-3 items-start"><input type="checkbox" className="mt-1" checked={selected.has(entry.id)} disabled={busy || !!entry.error} onChange={() => toggle(entry.id)} />
          <span className="min-w-0 text-sm break-all"><span className="block font-medium">{entry.source}</span><span className="block mt-1">{entry.rule} · {t(`beta.${entry.action}`)} · {entry.size.toLocaleString()} B</span>{entry.destination && <span className="block text-text-muted">{entry.destination}</span>}</span>
        </label>
        {entry.warning && <p className="mt-2 text-sm text-amber-800 dark:text-amber-200">{t('beta.warning')}</p>}
        {entry.error && <p className="mt-2 text-sm text-text-muted">{entry.error === 'preview.recoveryUnavailable' ? t('beta.recoveryUnavailable') : errorText(entry.error)}</p>}
      </div>)}</div>
      <button className={primaryClass} disabled={busy || !selected.size} onClick={(event) => askApply(event.currentTarget)}>{t('beta.apply')}</button>
    </>}
    {confirm && <ConfirmDialog {...confirm} onClose={() => setConfirm((active) => active === confirm ? null : active)} />}
  </section>;
}
