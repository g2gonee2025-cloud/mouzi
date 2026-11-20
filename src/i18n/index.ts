import i18n, { type ResourceLanguage } from 'i18next';
import { initReactI18next } from 'react-i18next';
import en from './locales/en.json';

export type SupportedLang = 'en' | 'pl' | 'it' | 'de' | 'fr' | 'ru' | 'ja' | 'vi' | 'es' | 'uk' | 'zh-CN';

const loadLocales: Partial<Record<string, () => Promise<ResourceLanguage>>> =
  import.meta.glob<ResourceLanguage>(['./locales/*.json', '!./locales/en.json'], { import: 'default' });
let initialized: ReturnType<typeof i18n.init> | undefined;
let languageRequest = 0;

export async function initI18n(lang: SupportedLang) {
  const request = ++languageRequest;
  await (initialized ??= i18n.use(initReactI18next).init({
    resources: { en: { translation: en } },
    lng: 'en',
    fallbackLng: 'en',
    interpolation: { escapeValue: false },
  }));

  const load = loadLocales[`./locales/${lang}.json`];
  let language: SupportedLang = load ? lang : 'en';
  if (load && !i18n.hasResourceBundle(lang, 'translation')) {
    try {
      const translation = await load();
      if (request === languageRequest) i18n.addResourceBundle(lang, 'translation', translation);
    } catch (error) {
      console.error(`Could not load the ${lang} translation`, error);
      language = 'en';
    }
  }

  if (request === languageRequest) await i18n.changeLanguage(language);
  return i18n.resolvedLanguage ?? 'en';
}

export default i18n;
