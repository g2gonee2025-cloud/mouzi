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

const unreadable = {
  root: "C:/Fixture/Policy folder",
  message: "Unable to read ignore protection file C:/Fixture/Policy folder/.mouziignore",
};
const offline = {
  root: "D:/Fixture/Offline",
  message: "The watched folder is unavailable",
};

function deferredSuggestions() {
  let finish!: (response: SuggestionsResponse) => void;
  const result = new Promise<SuggestionsResponse>((resolve) => { finish = resolve; });
  const resolve = (
    suggestions: Suggestion[],
    localAiStatus: SuggestionsResponse["localAiStatus"] = null,
    policyIssues?: Array<typeof unreadable>,
  ) => {
    finish({ suggestions, localAiStatus, ...(policyIssues ? { policyIssues } : {}) });
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

it("distinguishes a fresh failed lookup from the next verified empty read", async () => {
  expect.soft(renderScreen().html).not.toContain("suggestions.empty");
  mocks.invoke.mockRejectedValueOnce("Could not read fixture inventory");
  await useSuggestionsStore.getState().loadSuggestions();
  const failure = renderScreen().html;
  expect.soft(failure).toContain("suggestions.readFailed");
  expect.soft(failure).toContain("common.retry");
  expect.soft(failure).toContain("Could not read fixture inventory");
  expect.soft(failure).not.toContain("suggestions.empty");

  mocks.invoke.mockResolvedValueOnce({ suggestions: [], localAiStatus: null });
  await useSuggestionsStore.getState().loadSuggestions();
  const verified = renderScreen().html;
  expect(verified).toContain("suggestions.empty");
  expect(verified).not.toContain("suggestions.readFailed");
});

it("reports skipped policy folders while healthy suggestions and existing feedback stay usable", async () => {
  const completed = "C:/Filed/completed.txt";
  useSuggestionsStore.setState({
    results: [{ path: "C:/Fixture/completed.txt", status: "ok", message: null, dest: completed }],
    error: "The separate dismissal could not be saved",
  });
  mocks.invoke.mockResolvedValueOnce({
    suggestions: [receipt], localAiStatus: "modelMissing", policyIssues: [unreadable, offline],
  });
  const store = useSuggestionsStore.getState();
  await store.loadSuggestions();

  const { html } = renderScreen();
  expect.soft(html).toMatch(/role="status"[^>]*><p[^>]*>suggestions.policyReadIncomplete/);
  for (const issue of [unreadable, offline]) {
    expect.soft(html).toContain(issue.root);
    expect.soft(html).toContain(issue.message);
  }
  expect.soft(html).toContain("suggestions.localAiModelMissing");
  expect.soft(html).toContain("The separate dismissal could not be saved");
  expect.soft(html).toContain(completed);
  expect.soft(html).not.toContain("suggestions.readFailed");
  expect(html).toContain(receipt.filename);
  expect(html).not.toMatch(/<button disabled=""[^>]+aria-label="suggestions.accept"/);

  mocks.invoke.mockResolvedValueOnce("fixture-preparation").mockResolvedValueOnce([{
    planId: "fixture-plan", path: receipt.path, category: receipt.suggestedCategory,
    destination: "C:/Filed/receipt.txt", size: 1, rule: null,
  }]);
  const plans = await store.prepareSuggestions([receipt], false);
  await store.loadSuggestions();
  expect(useSuggestionsStore.getState().preparedPlans).toBe(plans);
  expect(mocks.invoke.mock.calls.filter(([command]) => command === "get_suggestions_with_status_cmd")).toHaveLength(1);
});

it("uses the partial notice instead of ordinary empty copy and clears it after a healthy retry", async () => {
  const refresh = useSuggestionsStore.getState().loadSuggestions;
  mocks.invoke.mockResolvedValueOnce({ suggestions: [], localAiStatus: null, policyIssues: [unreadable] });
  await refresh();
  const partial = renderScreen().html;
  expect.soft(partial).toContain("suggestions.policyReadIncomplete");
  expect.soft(partial).not.toContain("suggestions.empty");
  expect.soft(partial).not.toContain("suggestions.hint");

  const recovered = deferredSuggestions();
  mocks.invoke.mockReturnValueOnce(recovered.result);
  const request = refresh();
  expect(renderScreen().html).not.toContain(unreadable.root);
  recovered.resolve([]);
  await request;
  const verified = renderScreen().html;
  expect(verified).toContain("suggestions.empty");
  expect(verified).not.toContain("suggestions.policyReadIncomplete");
});

it("ignores an older policy diagnostic after the next healthy Suggestions read", async () => {
  const stale = deferredSuggestions();
  const refresh = useSuggestionsStore.getState().loadSuggestions;
  mocks.invoke.mockReturnValueOnce(stale.result).mockResolvedValueOnce({
    suggestions: [receipt], localAiStatus: null, policyIssues: [],
  });
  const oldRequest = refresh();
  await refresh();
  stale.resolve([], null, [unreadable]);
  await oldRequest;
  const { html } = renderScreen();
  expect(html).toContain(receipt.filename);
  expect(html).not.toContain(unreadable.root);
  expect(html).not.toContain("suggestions.policyReadIncomplete");
});

it("retains a committed receipt and its mutation error through a later lookup failure", async () => {
  useSuggestionsStore.setState({
    results: [{ path: "C:/Fixture/completed.txt", status: "partial", message: "Receipt warning", dest: "C:/Filed/completed.txt" }],
    error: "A mutation could not be recorded",
  });
  mocks.invoke.mockRejectedValueOnce("Could not read fixture inventory");
  await useSuggestionsStore.getState().loadSuggestions();
  const failed = renderScreen().html;
  expect(failed).toContain("C:/Filed/completed.txt");
  expect(failed).toContain("Receipt warning");
  expect.soft(failed).toContain("A mutation could not be recorded");
  expect.soft(failed).toContain("suggestions.readFailed");

  mocks.invoke.mockResolvedValueOnce({ suggestions: [receipt], localAiStatus: "modelMissing" });
  await useSuggestionsStore.getState().loadSuggestions();
  const recovered = renderScreen().html;
  expect(recovered).toContain("C:/Filed/completed.txt");
  expect(recovered).toContain("receipt.unrecognized");
  expect(recovered).toContain("suggestions.localAiModelMissing");
  expect(recovered).toContain("A mutation could not be recorded");
});

it("labels mixed accept receipts with each actual outcome and shows each reason once", async () => {
  const extra = (name: string): Suggestion => ({
    ...invoice, path: `C:/Fixture/${name}.unrecognized`, filename: `${name}.unrecognized`,
  });
  const refused = extra("refused");
  const filed = extra("already-filed");
  const missing = extra("missing");
  const selected = [invoice, receipt, refused, filed, missing];
  const actual = "C:/Filed/actual-invoice.unrecognized";
  const movedWarning = "File moved; future rule was not created";
  const retainedWarning = "Output retained; inspect both locations";
  const moveFailure = "Move refused before publication";
  const distinctError = "Could not write the separate dismissal";
  const store = useSuggestionsStore.getState();
  mocks.invoke.mockResolvedValueOnce({ suggestions: selected, localAiStatus: null });
  await store.loadSuggestions();
  mocks.invoke.mockResolvedValueOnce("fixture-preparation").mockResolvedValueOnce(selected.map((card, i) => ({
    planId: `fixture-${i}`, path: card.path, category: card.suggestedCategory,
    destination: `C:/Proposed/${card.filename}`, size: 1, rule: null,
  })));
  const reviewed = await store.prepareSuggestions(selected, true);
  mocks.invoke
    .mockResolvedValueOnce({ path: invoice.path, status: "partial", dest: actual, message: movedWarning })
    .mockResolvedValueOnce({ path: receipt.path, status: "partial", dest: null, message: retainedWarning })
    .mockRejectedValueOnce(moveFailure)
    .mockResolvedValueOnce({ path: filed.path, status: "ok", dest: filed.path, message: "Future rule saved" })
    .mockResolvedValueOnce({ path: missing.path, status: "missing", dest: null, message: null });
  await store.acceptAll(reviewed);

  expect(useSuggestionsStore.getState().suggestions).toEqual([receipt, refused]);
  expect(useSuggestionsStore.getState().results?.[0].dest).toBe(actual);
  const { html } = renderScreen();
  expect.soft(html).toContain(actual);
  expect.soft(html.slice(html.indexOf(actual), html.indexOf(receipt.path))).toContain("suggestions.movedWithWarning");
  expect.soft(html.slice(html.indexOf(receipt.path), html.indexOf(refused.path))).toContain("suggestions.moveNeedsReview");
  expect.soft(html.split("suggestions.failed")).toHaveLength(2);
  expect(html).toContain("suggestions.alreadyFiled");
  expect(html).toContain("suggestions.missing");
  for (const reason of [movedWarning, retainedWarning, moveFailure, "Future rule saved"]) {
    expect.soft(html.split(reason)).toHaveLength(2);
  }

  mocks.invoke.mockRejectedValueOnce(distinctError);
  await store.dismissSuggestions([receipt]);
  const subsequent = renderScreen().html;
  expect(subsequent.split(distinctError)).toHaveLength(2);
  expect(subsequent.split(movedWarning)).toHaveLength(2);
  expect(subsequent).toContain(actual);
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

  it("distinguishes a failed generation while keeping independent results usable", async () => {
    const classified: Suggestion = { ...receipt, source: "ollama" };
    const refresh = useSuggestionsStore.getState().loadSuggestions;
    mocks.invoke.mockResolvedValueOnce({
      suggestions: [invoice, classified], localAiStatus: "generationFailed",
    });
    await refresh();
    const { html } = renderScreen();
    expect.soft(html).toContain("suggestions.localAiGenerationFailed");
    expect.soft(html).not.toContain("suggestions.localAiUnreachable");
    expect(html).not.toContain("suggestions.localAiModelMissing");
    expect(html).toContain("invoice.unrecognized");
    expect(html).toContain("receipt.unrecognized");
    expect(html).toContain("suggestions.source.ollama");
    expect(html).not.toMatch(/<button disabled=""[^>]+aria-label="suggestions.accept"/);

    nextLookup([classified], null);
    await refresh();
    expect(renderScreen().html).not.toContain("suggestions.localAiGenerationFailed");
  });
});
