import { describe, expect, it } from "vitest";

const locales = import.meta.glob("../i18n/locales/*.json", { eager: true, import: "default" });
const keys = [
  "common.retry", "common.save", "common.saving", "common.loading", "common.dismiss", "common.close", "common.back",
  "settings.general.localAi", "settings.general.localAiModel", "settings.general.localAiPrivacy", "settings.general.gracePeriodUnit",
  "common.cancel", "settings.archive.progress", "suggestions.alreadyFiled",
  "settings.history.showing", "dashboard.loadingFiles", "dashboard.rootPending", "dashboard.rootStale",
  "dashboard.rootFailed", "dashboard.rootLastScan", "dashboard.scanFailed",
  "settings.ignore.extensionTip", "settings.ignore.folderTip", "settings.ignore.officeTip", "settings.ignore.exactTip",
  "cleanup.historyLoading", "cleanup.historyError", "cleanup.notSearched", "cleanup.historyUnknown",
  ...["resetDismissed", "rulePreview", "ruleScope", "rulePriority", "ruleExtension", "ruleDestination", "ruleAlreadyCovered", "ruleConflict", "ruleNotApplicable", "partial", "preparing", "ruleCount", "planChanged"].map((name) => `suggestions.${name}`),
  ...["Documents", "Images", "Videos", "Audio", "Archives", "Code", "Other"].map((name) => `categories.${name}`),
];

function translation(locale: unknown, key: string): unknown {
  return key.split(".").reduce<unknown>((value, part) => {
    if (!value || typeof value !== "object") return undefined;
    return Reflect.get(value, part);
  }, locale);
}

describe("preferences and inventory translations", () => {
  it("provides every new UI key in all ten shipped locales", () => {
    expect(Object.keys(locales)).toHaveLength(10);
    for (const [path, locale] of Object.entries(locales)) {
      for (const key of keys) {
        const value = translation(locale, key);
        expect(value, `${path}: ${key}`).toBeTypeOf("string");
        expect(String(value).trim(), `${path}: ${key}`).not.toBe("");
      }
    }
  });

  it("preserves the data placeholders needed by history and root summaries", () => {
    for (const locale of Object.values(locales)) {
      expect(translation(locale, "settings.history.showing")).toContain("{{shown}}");
      expect(translation(locale, "settings.history.showing")).toContain("{{total}}");
      expect(translation(locale, "dashboard.rootLastScan")).toContain("{{date}}");
    }
  });
});
