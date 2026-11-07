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
