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
const pluralGroups = new Set(Object.keys(reference).filter(key => pluralSuffix.test(key))
  .map(key => key.replace(pluralSuffix, '')));
function messageKey(key) {
  const base = key.replace(pluralSuffix, '');
  return pluralGroups.has(base) ? base : key;
}
const messageKeys = entries => [...new Set(Object.keys(entries).map(messageKey))].sort();

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

function integerPluralExamples(language) {
  const rules = new Intl.PluralRules(language);
  const examples = new Map();
  // The UI passes integer file, rule, and history counts. A million also
  // exercises the additional category used by several Romance languages.
  for (const count of [...Array.from({ length: 200 }, (_, n) => n), 1_000_000]) {
    const category = rules.select(count);
    if (!examples.has(category)) examples.set(category, count);
  }
  return examples;
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
    assert.deepEqual(messageKeys(translated), messageKeys(reference));
    for (const [key, value] of Object.entries(translated)) {
      const base = messageKey(key);
      const source = reference[key] ?? reference[`${base}_other`];
      assert.ok(value.trim(), `${language}: empty ${key}`);
      assert.deepEqual(tokens(value), tokens(source), `${language}: placeholders in ${key}`);
    }
    // Disable fallback so a missing runtime lookup cannot silently use English.
    const instance = i18next.createInstance();
    await instance.init({ lng: language, fallbackLng: false, resources: { [language]: { translation: locale } } });
    const examples = integerPluralExamples(language);
    for (const key of messageKeys(reference)) {
      if (!pluralGroups.has(key)) {
        assert.equal(instance.exists(key), true, key);
        assert.equal(instance.t(key, { count: 3 }), translated[key].replaceAll('{{count}}', '3'));
        continue;
      }
      for (const [category, count] of examples) {
        const variant = count === 0 && Object.hasOwn(translated, `${key}_zero`)
          ? `${key}_zero` : `${key}_${category}`;
        const expected = translated[variant] ?? (examples.size === 1 ? translated[key] : undefined);
        assert.ok(expected, `${language}: missing ${variant}`);
        assert.equal(instance.t(key, { count }), expected.replaceAll('{{count}}', String(count)));
      }
    }
  });
}
