import { useTranslation } from 'react-i18next';
import { defaultRuleOptions, Rule } from '../store/useAppStore';
import i18n from '../i18n';

export const fieldClass = 'mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-sm';
export const buttonClass = 'rounded-md border border-border px-3 py-2 text-sm hover:bg-surface-dark disabled:opacity-50 disabled:cursor-not-allowed';
export const primaryClass = 'rounded-md bg-primary px-3 py-2 text-sm font-semibold text-stone-950 disabled:opacity-50 disabled:cursor-not-allowed';
export function errorText(error: unknown): string {
  const raw = String(error);
  const match = raw.match(/validation\.[a-z]+/);
  if (match) return i18n.t(`beta.${match[0]}`);
  return i18n.t(`beta.errors.${raw}`, { defaultValue: raw });
}
export interface OperationResult {
  id: string;
  source: string;
  success: boolean;
  error: string | null;
  actualDestination?: string;
}
export function Results({ results }: { results: OperationResult[] }) {
  const { t } = useTranslation();
  if (!results.length) return null;
  return <div role="status" className="space-y-2 rounded-lg border border-border p-3">
    <h3 className="font-medium">{t('beta.results')}</h3>
    {results.map(result => <div key={result.id} className="text-sm break-all">
      <strong>{t(result.success ? 'beta.done' : result.actualDestination ? 'beta.movedWithHistoryWarning' : 'beta.failed')}</strong>: {result.source}
      {result.actualDestination && <p><span className="text-text-muted">{t('beta.destination')}</span> {result.actualDestination}</p>}
      {result.error && <p className="text-red-700 dark:text-red-300">{errorText(result.error)}</p>}
    </div>)}
  </div>;
}
export function RuleConditions({ rule, onChange }: { rule: Rule; onChange: (rule: Rule) => void }) {
  const { t } = useTranslation();
  const options = { ...defaultRuleOptions, ...rule.options };
  return <fieldset className="rounded-md border border-border p-3 space-y-3">
    <legend className="px-1 text-sm font-medium">{t('beta.conditions')}</legend>
    <p className="text-xs text-text-muted">{t('beta.conditionHelp')}</p>
    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
      {(['min_size', 'max_size'] as const).map((key, index) => <label key={key} className="text-sm">
        {t(index ? 'beta.maxSize' : 'beta.minSize')}
        <input type="number" min="0" step="1" className={fieldClass} value={options[key] ?? ''} onChange={e => onChange({ ...rule, options: { ...options, [key]: e.target.value === '' ? null : Number(e.target.value) } })} />
      </label>)}
      {(['modified_after', 'modified_before'] as const).map((key, index) => <label key={key} className="text-sm">
        {t(index ? 'beta.before' : 'beta.after')}
        <input type="date" className={fieldClass} value={options[key] ?? ''} onChange={e => onChange({ ...rule, options: { ...options, [key]: e.target.value || null } })} />
      </label>)}
    </div>
    {rule.action === 'rename' && <label className="block text-sm">{t('beta.template')}
      <input className={fieldClass} value={options.rename_template} onChange={e => onChange({ ...rule, options: { ...options, rename_template: e.target.value } })} />
      <span className="mt-1 block text-xs text-text-muted">{t('beta.templateHelp')}</span>
    </label>}
  </fieldset>;
}
