import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import i18next from 'i18next';

const directory = new URL('../src/i18n/locales/', import.meta.url);
const locales = Object.fromEntries(await Promise.all(
  (await readdir(directory)).filter(file => file.endsWith('.json')).map(async file => [
    file.slice(0, -5), JSON.parse(await readFile(new URL(file, directory), 'utf8')),
  ]),
));
function strings(object, prefix = '') {
  return Object.fromEntries(Object.entries(object).flatMap(([key, value]) => {
    if (!prefix && key === '__translator_info__') return [];
    const path = prefix ? `${prefix}.${key}` : key;
    return typeof value === 'string' ? [[path, value]] : Object.entries(strings(value, path));
  }));
}
const tokens = value => [...value.matchAll(/\{\{[^{}]+\}\}|\{[^{}]+\}/g)].map(match => match[0]).sort();
const reference = strings(locales.en);
const pluralSuffix = /_(zero|one|two|few|many|other)$/;
const referencePlurals = new Set(Object.keys(reference).filter(key => pluralSuffix.test(key))
  .map(key => key.replace(pluralSuffix, '')));
// Keep requiring this family even if every localized variant is removed.
const requiredLocalPlurals = { de: ['settings.scheduler.scheduledTimes'] };
function messageKey(key, pluralGroups) {
  const base = key.replace(pluralSuffix, '');
  return pluralGroups.has(base) ? base : key;
}
const messageKeys = (entries, pluralGroups) => [...new Set(
  Object.keys(entries).map(key => messageKey(key, pluralGroups)),
)].sort();
const referenceKeys = messageKeys(reference, referencePlurals);

// These are the words the person approving the selection should see.
const selectionCaptions = {
  de: [[1, '1 Aktion ausführen', '1 Datei wiederherstellen'], [2, '2 Aktionen ausführen', '2 Dateien wiederherstellen']],
  en: [[1, 'Apply 1 operation', 'Restore 1 file'], [2, 'Apply 2 operations', 'Restore 2 files']],
  es: [[1, 'Aplicar 1 operación', 'Restaurar 1 archivo'], [2, 'Aplicar 2 operaciones', 'Restaurar 2 archivos']],
  fr: [[1, 'Appliquer 1 opération', 'Restaurer 1 fichier'], [2, 'Appliquer 2 opérations', 'Restaurer 2 fichiers']],
  it: [[1, 'Applica 1 operazione', 'Ripristina 1 file'], [2, 'Applica 2 operazioni', 'Ripristina 2 file']],
  ja: [[1, '1 件の操作を実行', '1 件のファイルを復元'], [2, '2 件の操作を実行', '2 件のファイルを復元']],
  pl: [
    [1, 'Wykonaj 1 operację', 'Przywróć 1 plik'],
    [2, 'Wykonaj 2 operacje', 'Przywróć 2 pliki'],
    [5, 'Wykonaj 5 operacji', 'Przywróć 5 plików'],
    [21, 'Wykonaj 21 operacji', 'Przywróć 21 plików'],
  ],
  ru: [
    [1, 'Выполнить 1 операцию', 'Восстановить 1 файл'],
    [2, 'Выполнить 2 операции', 'Восстановить 2 файла'],
    [5, 'Выполнить 5 операций', 'Восстановить 5 файлов'],
    [21, 'Выполнить 21 операцию', 'Восстановить 21 файл'],
  ],
  uk: [
    [1, 'Виконати 1 операцію', 'Відновити 1 файл'],
    [2, 'Виконати 2 операції', 'Відновити 2 файли'],
    [5, 'Виконати 5 операцій', 'Відновити 5 файлів'],
    [21, 'Виконати 21 операцію', 'Відновити 21 файл'],
  ],
  vi: [[1, 'Thực hiện 1 thao tác', 'Khôi phục 1 tệp'], [2, 'Thực hiện 2 thao tác', 'Khôi phục 2 tệp']],
  'zh-CN': [[1, '执行 1 项操作', '恢复 1 个文件'], [2, '执行 2 项操作', '恢复 2 个文件']],
};

function integerPluralExamples(rules) {
  const examples = new Map();
  // The UI passes integer file, rule, and history counts. A million also
  // exercises the additional category used by several Romance languages.
  for (const count of [...Array.from({ length: 200 }, (_, n) => n + 1), 0, 1_000_000]) {
    const category = rules.select(count);
    if (!examples.has(category)) examples.set(category, count);
  }
  return examples;
}

function interpolationSample(value, count, numbers) {
  const fields = { count };
  const rendered = value.replace(/\{\{\s*([^,{}]+?)\s*(?:,\s*([^{}]+?)\s*)?\}\}/g, (_, field, format) => {
    fields[field] ??= 1_234 + Object.keys(fields).length;
    assert.ok(format === undefined || format === 'number', `Unsupported test format: ${format}`);
    return format === 'number' ? numbers.format(fields[field]) : String(fields[field]);
  });
  return { fields, rendered };
}

for (const [language, locale] of Object.entries(locales)) {
  test(language + ': review and restore confirmations name the exact selection', async () => {
    const instance = i18next.createInstance();
    await instance.init({ lng: language, fallbackLng: false, resources: { [language]: { translation: locale } } });
    const expected = selectionCaptions[language];
    const actual = expected.map(([count]) => [count, instance.t('beta.confirm', { count }), instance.t('beta.confirmUndo', { count })]);
    assert.deepEqual(actual, expected);
  });

  test(`${language}: all UI keys and placeholders are present without fallback (#67)`, async () => {
    assert.ok(locale.beta, 'Missing organization translations');
    const translated = strings(locale);
    const pluralGroups = new Set([...referencePlurals, ...(requiredLocalPlurals[language] ?? [])]);
    for (const key of Object.keys(translated)) {
      const base = key.replace(pluralSuffix, '');
      if (base !== key && Object.hasOwn(reference, base)) pluralGroups.add(base);
    }
    assert.deepEqual(messageKeys(translated, pluralGroups), referenceKeys);
    for (const [key, value] of Object.entries(translated)) {
      const base = messageKey(key, pluralGroups);
      const source = reference[key] ?? reference[`${base}_other`] ?? reference[base];
      assert.ok(value.trim(), `${language}: empty ${key}`);
      assert.deepEqual(tokens(value), tokens(source), `${language}: placeholders in ${key}`);
    }
    // Disable fallback so a missing runtime lookup cannot silently use English.
    const instance = i18next.createInstance();
    await instance.init({ lng: language, fallbackLng: false, resources: { [language]: { translation: locale } } });
    const rules = new Intl.PluralRules(language);
    const numbers = new Intl.NumberFormat(language);
    const examples = integerPluralExamples(rules);
    const check = (key, variant, count) => {
      const { fields, rendered } = interpolationSample(translated[variant], count, numbers);
      const result = instance.t(key, { ...fields, returnDetails: true });
      assert.equal(result.exactUsedKey, variant, `${language}: ${key} form at count ${count}`);
      assert.equal(result.res, rendered, `${language}: ${variant} text at count ${count}`);
    };
    for (const key of referenceKeys) {
      if (!referencePlurals.has(key)) {
        assert.ok(Object.hasOwn(translated, key) && instance.exists(key), `${language}: missing ${key}`);
      }
      if (!pluralGroups.has(key)) {
        check(key, key, 1_234);
        continue;
      }
      const customZero = Object.hasOwn(translated, `${key}_zero`);
      const counts = new Set([...examples.values(), 1_234, ...(customZero ? [0] : [])]);
      const permitsBase = referencePlurals.has(key) && examples.size === 1;
      for (const count of counts) {
        const suffix = count === 0 && customZero ? 'zero' : rules.select(count);
        const variant = `${key}_${suffix}`;
        const available = Object.hasOwn(translated, variant);
        assert.ok(available || (permitsBase && Object.hasOwn(translated, key)), `${language}: missing ${variant}`);
        check(key, available ? variant : key, count);
      }
    }
  });
}
