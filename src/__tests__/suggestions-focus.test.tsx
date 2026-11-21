import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import Suggestions from "../pages/Suggestions";
import { useSuggestionsStore, type Suggestion, type SuggestionsResponse } from "../store/useSuggestionsStore";

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
  version: { identity: "invoice-file", size: "1", mtimeNs: "1" },
};
const receipt: Suggestion = {
  ...invoice, path: "C:/Fixture/receipt.unrecognized", filename: "receipt.unrecognized", source: "learned",
};

function deferredSuggestions() {
  let finish!: (response: SuggestionsResponse) => void;
  const result = new Promise<SuggestionsResponse>((resolve) => { finish = resolve; });
  const resolve = (suggestions: Suggestion[], localAiStatus: SuggestionsResponse["localAiStatus"] = null) => {
    finish({ suggestions, localAiStatus });
  };
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
  expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith("get_suggestions_with_status_cmd", { limit: 100 });

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
  stale.resolve([invoice], "modelMissing");
  await first;

  const settled = renderScreen().html;
  expect(settled).toContain("receipt.unrecognized");
  expect(settled).not.toContain("invoice.unrecognized");
  expect(settled).not.toContain("suggestions.localAiModelMissing");
});

describe("local AI diagnostics", () => {
  function nextLookup(suggestions: Suggestion[], localAiStatus: SuggestionsResponse["localAiStatus"]) {
    mocks.invoke.mockImplementationOnce(async (command: string) => {
      if (command === "get_suggestions_cmd") return suggestions;
      if (command === "get_suggestions_with_status_cmd") return { suggestions, localAiStatus };
      throw new Error(`Unexpected command ${command}`);
    });
  }

  it("shows a confirmed missing model alongside an actionable heuristic card", async () => {
    nextLookup([invoice], "modelMissing");
    await useSuggestionsStore.getState().loadSuggestions();

    const { html } = renderScreen();
    expect(html).toContain('role="status"');
    expect(html).toContain("suggestions.localAiModelMissing");
    expect(html).toContain("invoice.unrecognized");
    expect(html).toContain("suggestions.source.heuristic");
    expect(html).toContain('aria-label="suggestions.accept"');
    expect(html).not.toMatch(/<button disabled=""[^>]+aria-label="suggestions.accept"/);
  });

  it("shows a confirmed unreachable service with the ordinary empty state and clears the note after recovery", async () => {
    const refresh = useSuggestionsStore.getState().loadSuggestions;
    nextLookup([], "unreachable");
    await refresh();

    const offline = renderScreen().html;
    expect(offline).toContain("suggestions.localAiUnreachable");
    expect(offline).toContain("suggestions.empty");
    expect(offline).toContain("suggestions.hint");

    nextLookup([receipt], null);
    const recovery = refresh();
    expect(renderScreen().html).not.toContain("suggestions.localAiUnreachable");
    await recovery;
    expect(renderScreen().html).not.toContain("suggestions.localAiUnreachable");
    expect(renderScreen().html).toContain("receipt.unrecognized");
  });

  it("does not infer a model failure from an empty result without provider evidence", async () => {
    nextLookup([], null);
    await useSuggestionsStore.getState().loadSuggestions();

    const { html } = renderScreen();
    expect(html).toContain("suggestions.empty");
    expect(html).not.toContain("suggestions.localAiModelMissing");
    expect(html).not.toContain("suggestions.localAiUnreachable");
  });
});
