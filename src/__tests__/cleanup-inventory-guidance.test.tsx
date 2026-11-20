import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { EffectCallback } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Cleanup from "../pages/Cleanup";
import { useCleanupStore } from "../store/useCleanupStore";

const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  effects: [] as EffectCallback[],
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: ipc.listen }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: (effect: EffectCallback) => { ipc.effects.push(effect); },
}));
vi.mock("../store/useCleanupStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useCleanupStore")>();
  const store = actual.useCleanupStore;
  const useSnapshot = (selector?: (state: ReturnType<typeof store.getState>) => unknown) => {
    const state = store.getState();
    return selector ? selector(state) : state;
  };
  return { ...actual, useCleanupStore: Object.assign(useSnapshot, store) };
});

const readinessCommand = "has_cleanup_inventory_cmd";
const hint = "cleanup.scanFirstHint";
const listeners = new Map<string, () => void>();
const disconnect: Array<() => void> = [];

function deferred<T>() {
  let resolve!: (value: T) => void;
  const result = new Promise<T>((finish) => { resolve = finish; });
  return { resolve, result };
}

const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function render(tab: string) {
  window.location.hash = `#/cleanup?tab=${tab}`;
  const html = renderToStaticMarkup(<Cleanup />);
  return { html, effects: ipc.effects.splice(0) };
}

function leave() {
  disconnect.splice(0).forEach((stop) => stop());
}

async function enter(tab: string) {
  leave();
  const { html, effects } = render(tab);
  for (const effect of effects) {
    const stop = effect();
    if (stop) disconnect.push(stop);
  }
  await settle();
  return html;
}

async function publish(event: string) {
  listeners.get(event)?.();
  await settle();
}

beforeEach(() => {
  ipc.invoke.mockReset();
  ipc.listen.mockImplementation(async (event: string, onEvent: () => void) => {
    listeners.set(event, onEvent);
    return () => { listeners.delete(event); };
  });
  useCleanupStore.setState(useCleanupStore.getInitialState(), true);
  vi.stubGlobal("window", Object.assign(new EventTarget(), { location: { hash: "" } }));
});

afterEach(() => {
  leave();
  listeners.clear();
  ipc.listen.mockReset();
  vi.unstubAllGlobals();
});

it.each([
  ["history", "cleanup.historyTitle"],
  ["emptyDirs", "cleanup.findEmptyDirs"],
])("opens %s directly without requiring or checking an inventory", async (tab, content) => {
  ipc.invoke.mockResolvedValue([]);
  const initial = await enter(tab);

  expect(initial).toContain(content);
  expect(initial).not.toContain(hint);
  expect(render(tab).html).not.toContain(hint);
  expect(ipc.invoke.mock.calls.map(([command]) => command)).not.toContain(readinessCommand);
  expect(ipc.listen).not.toHaveBeenCalled();
});

it.each(["duplicates", "large", "stale"])("shows a confirmed prerequisite on direct %s entry only after the read", async (tab) => {
  ipc.invoke.mockResolvedValue(false);
  const initial = await enter(tab);

  expect(initial).not.toContain(hint);
  expect(ipc.invoke.mock.calls).toEqual([[readinessCommand]]);
  expect(render(tab).html).toContain(hint);
});

it("keeps the file prerequisite after loading an empty Cleanup History", async () => {
  ipc.invoke.mockImplementation(async (command: string) => command === readinessCommand ? false : []);
  await enter("history");
  expect(render("history").html).toContain("cleanup.historyEmpty");
  await enter("duplicates");
  expect(render("duplicates").html).toContain(hint);
});

it.each(["an empty completed scan", "prior inventory"])("trusts readiness for %s before any finder has run", async () => {
  ipc.invoke.mockResolvedValue(true);
  await enter("large");

  expect(render("large").html).toContain("cleanup.findLarge");
  expect(render("large").html).not.toContain(hint);
  expect(ipc.invoke.mock.calls).toEqual([[readinessCommand]]);
});

it("registers for inventory changes before reading a direct entry snapshot", async () => {
  const delayed = deferred<() => void>();
  ipc.listen.mockImplementationOnce(() => delayed.result);
  ipc.invoke.mockResolvedValue(false);
  await enter("stale");
  expect(ipc.invoke).not.toHaveBeenCalled();

  delayed.resolve(vi.fn());
  await settle();
  expect(ipc.invoke.mock.calls).toEqual([[readinessCommand]]);
  expect(render("stale").html).toContain(hint);
});

it("uses scan and folder events while visible, and discards stale or failed reads", async () => {
  const oldRead = deferred<boolean>();
  ipc.invoke.mockReturnValueOnce(oldRead.result)
    .mockResolvedValueOnce(true).mockResolvedValueOnce(true).mockResolvedValueOnce(false)
    .mockRejectedValueOnce("Fixture database unavailable");
  await enter("duplicates");

  await publish("scan-complete");
  oldRead.resolve(false);
  await settle();
  expect(render("duplicates").html).not.toContain(hint);

  await publish("scan-finished");
  expect(render("duplicates").html).not.toContain(hint);
  await publish("folders-changed");
  expect(render("duplicates").html).toContain(hint);
  await publish("mutation-completed");
  expect(render("duplicates").html).not.toContain(hint);
  expect(ipc.invoke.mock.calls).toEqual(Array.from({ length: 5 }, () => [readinessCommand]));

  await enter("emptyDirs");
  await publish("folders-changed");
  expect(ipc.invoke).toHaveBeenCalledTimes(5);
});
