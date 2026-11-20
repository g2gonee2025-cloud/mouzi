import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import Suggestions from "../pages/Suggestions";
import { useSuggestionsStore, type Suggestion } from "../store/useSuggestionsStore";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  effects: [] as Array<() => void | (() => void)>,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: (effect: () => void | (() => void)) => { mocks.effects.push(effect); },
}));
vi.mock("../store/useSuggestionsStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useSuggestionsStore")>();
  const store = actual.useSuggestionsStore;
  return { ...actual, useSuggestionsStore: Object.assign(store.getState, store) };
});
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

const invoice: Suggestion = {
  path: "C:/Fixture/invoice.unrecognized", filename: "invoice.unrecognized",
  currentCategory: "other", suggestedCategory: "documents", confidence: 0.65, source: "heuristic",
};
const receipt: Suggestion = {
  ...invoice, path: "C:/Fixture/receipt.unrecognized", filename: "receipt.unrecognized", source: "learned",
};

function deferredSuggestions() {
  let resolve!: (suggestions: Suggestion[]) => void;
  const result = new Promise<Suggestion[]>((finish) => { resolve = finish; });
  return { resolve, result };
}

function renderScreen() {
  const html = renderToStaticMarkup(<Suggestions />);
  return { html, effects: mocks.effects.splice(0) };
}

const cleanups: Array<() => void> = [];

beforeEach(() => {
  mocks.invoke.mockReset();
  useSuggestionsStore.setState(useSuggestionsStore.getInitialState(), true);
  vi.stubGlobal("window", new EventTarget());
});

afterEach(() => {
  cleanups.splice(0).forEach((cleanup) => cleanup());
  vi.unstubAllGlobals();
});

it("settles the current lookup when the user switches back to Suggestions repeatedly", async () => {
  const initial = deferredSuggestions();
  const later = deferredSuggestions();
  mocks.invoke.mockReturnValueOnce(initial.result).mockReturnValue(later.result);
  for (const effect of renderScreen().effects) {
    const cleanup = effect();
    if (cleanup) cleanups.push(cleanup);
  }

  window.dispatchEvent(new Event("focus"));
  window.dispatchEvent(new Event("focus"));
  expect(renderScreen().html).toContain("app.loading");
  initial.resolve([invoice]);
  await initial.result;

  const settled = renderScreen().html;
  expect(settled).toContain("invoice.unrecognized");
  expect(settled).toContain("suggestions.source.heuristic");
  expect(settled).not.toMatch(/<button disabled=""[^>]+aria-label="suggestions.accept"/);
  expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith("get_suggestions_cmd", { limit: 100 });

  window.dispatchEvent(new Event("focus"));
  expect(renderScreen().html).toMatch(/<button disabled=""[^>]+aria-label="suggestions.accept"/);
  later.resolve([receipt]);
  await later.result;
  expect(renderScreen().html).toContain("suggestions.source.learned");
});

it("lets an inventory change replace an outstanding Suggestions lookup", async () => {
  const stale = deferredSuggestions();
  const fresh = deferredSuggestions();
  mocks.invoke.mockReturnValueOnce(stale.result).mockReturnValueOnce(fresh.result);

  const refresh = useSuggestionsStore.getState().loadSuggestions;
  const first = refresh();
  const afterMutation = refresh();
  fresh.resolve([receipt]);
  await afterMutation;
  stale.resolve([invoice]);
  await first;

  const settled = renderScreen().html;
  expect(settled).toContain("receipt.unrecognized");
  expect(settled).not.toContain("invoice.unrecognized");
});
