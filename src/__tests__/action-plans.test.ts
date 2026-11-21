import { beforeEach, expect, it, vi } from "vitest";
const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ipc);
import { useCleanupStore } from "../store/useCleanupStore";
import { useSuggestionsStore, distinctSuggestionRules, type Suggestion, type SuggestionPlan } from "../store/useSuggestionsStore";
import type { CleanupPlan } from "../utils/cleanup";

const suggestion = (path: string): Suggestion => ({ path, filename: "report.xyz", suggestedCategory: "Documents", currentCategory: "Other", confidence: 0.5, source: "heuristic", version: { identity: path, size: "1", mtimeNs: "1" } });
const plan = (path: string, category = "Documents"): SuggestionPlan => ({ path, planId: `plan:${path}`, category, destination: `root/${category}/report.xyz`, rule: { extension: "xyz", destination: category, scope: "All eligible roots", priority: 10, disposition: "create", message: null } });
const cleanupPlan: CleanupPlan = { planId: "cleanup:reviewed", expiresAt: "2026-10-03T12:00:00Z", actions: [{ kind: "trash_large", path: "a", minBytes: 100 }, { kind: "trash_large", path: "b", minBytes: 100 }], totalBytes: 200 };
const row = (path: string) => ({ path, size: 100, mtime: 1 });
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; }

function verifiedSuggestions(...paths: string[]) {
  const suggestions = paths.map(suggestion);
  useSuggestionsStore.setState({ suggestions, lookup: { status: "ready" } });
  return suggestions;
}

beforeEach(() => {
  ipc.invoke.mockReset();
  useCleanupStore.setState(useCleanupStore.getInitialState(), true);
  useSuggestionsStore.setState(useSuggestionsStore.getInitialState(), true);
});

it("previews cleanup criteria and executes only the reviewed backend token", async () => {
  useCleanupStore.setState({ largeFiles: { minBytes: 100, files: [row("a"), row("b")] } });
  ipc.invoke.mockResolvedValueOnce(cleanupPlan);
  const reviewed = await useCleanupStore.getState().prepareCleanup(cleanupPlan.actions);
  expect(ipc.invoke).toHaveBeenCalledWith("prepare_cleanup_cmd", { actions: cleanupPlan.actions });
  expect(ipc.invoke).not.toHaveBeenCalledWith("execute_cleanup_cmd", expect.anything());
  ipc.invoke.mockResolvedValueOnce([{ path: "a", status: "ok", message: "Recovery unverified" }, { path: "b", status: "failed", message: "File changed" }]).mockResolvedValueOnce([]);
  await useCleanupStore.getState().executeCleanup(reviewed.planId);
  expect(ipc.invoke).toHaveBeenCalledWith("execute_cleanup_cmd", { planId: "cleanup:reviewed" });
  expect(useCleanupStore.getState().largeFiles).toEqual({ minBytes: 100, files: [row("b")] });
  expect(useCleanupStore.getState().receipts.trash_large?.outcomes[1].message).toBe("File changed");
  useCleanupStore.getState().invalidateCandidates();
  expect(useCleanupStore.getState().receipts.trash_large?.outcomes).toHaveLength(2);
});

it("discards a late finder and refuses a preview invalidated during preparation", async () => {
  const older = deferred<{ candidates: ReturnType<typeof row>[]; unavailableRoots: string[] }>();
  ipc.invoke.mockReturnValueOnce(older.promise).mockResolvedValueOnce({ candidates: [row("current")], unavailableRoots: ["W:/current"] });
  const first = useCleanupStore.getState().findLargeFiles(10);
  await useCleanupStore.getState().findLargeFiles(20);
  older.resolve({ candidates: [row("old")], unavailableRoots: ["Z:/old"] }); await first;
  expect(useCleanupStore.getState().largeFiles).toEqual({ minBytes: 20, files: [row("current")] });
  expect(useCleanupStore.getState().unavailableRoots.largeFiles).toEqual(["W:/current"]);
  const pending = deferred<CleanupPlan>(); ipc.invoke.mockReturnValueOnce(pending.promise);
  const preparing = useCleanupStore.getState().prepareCleanup(cleanupPlan.actions);
  const rejection = expect(preparing).rejects.toThrow("candidates changed");
  useCleanupStore.getState().invalidateCandidates(); pending.resolve(cleanupPlan);
  await rejection;
  expect(ipc.invoke).toHaveBeenCalledWith("discard_cleanup_plan_cmd", { planId: cleanupPlan.planId });
  expect(useCleanupStore.getState().preparedPlan).toBeNull();
  expect(useCleanupStore.getState().largeFiles).toEqual({ minBytes: 20, files: [row("current")] });
  expect(useCleanupStore.getState().unavailableRoots.largeFiles).toEqual(["W:/current"]);
  useCleanupStore.getState().invalidateCandidates();
  expect(useCleanupStore.getState().largeFiles).toBeNull();
  expect(useCleanupStore.getState().unavailableRoots).toEqual({});
});

it("keeps omissions with each cleanup finder and clears them on a fresh search or scope invalidation", async () => {
  const store = useCleanupStore.getState();
  ipc.invoke.mockResolvedValueOnce({ candidates: [], unavailableRoots: ["D:/duplicates"] });
  await store.findDuplicates();
  ipc.invoke.mockResolvedValueOnce({ candidates: ["C:/empty"], unavailableRoots: ["E:/empty"] });
  await store.findEmptyDirs();
  expect(useCleanupStore.getState().unavailableRoots).toMatchObject({
    duplicates: ["D:/duplicates"],
    emptyDirs: ["E:/empty"],
  });
  expect(useCleanupStore.getState().emptyDirs).toEqual(["C:/empty"]);

  const rerun = deferred<never[]>();
  ipc.invoke.mockReturnValueOnce(rerun.promise);
  const complete = store.findDuplicates();
  expect(useCleanupStore.getState().unavailableRoots.duplicates).toEqual([]);
  rerun.resolve([]);
  await complete;
  expect(useCleanupStore.getState().duplicates).toEqual([]);
  expect(useCleanupStore.getState().unavailableRoots.emptyDirs).toEqual(["E:/empty"]);

  const late = deferred<{ candidates: string[]; unavailableRoots: string[] }>();
  ipc.invoke.mockReturnValueOnce(late.promise);
  const waiting = store.findEmptyDirs();
  store.invalidateCandidates();
  late.resolve({ candidates: [], unavailableRoots: ["F:/stale"] });
  await waiting;
  expect(useCleanupStore.getState().unavailableRoots).toEqual({});
  expect(useCleanupStore.getState().emptyDirs).toBeNull();
});

it("releases an abandoned cleanup review without letting its response replace a newer review", async () => {
  const abandoned = deferred<CleanupPlan>();
  const next = deferred<CleanupPlan>();
  ipc.invoke.mockReturnValueOnce(abandoned.promise).mockReturnValueOnce(next.promise);
  const firstOwner = new AbortController();
  const first = useCleanupStore.getState().prepareCleanup(cleanupPlan.actions, firstOwner.signal);
  const rejection = expect(first).rejects.toMatchObject({ name: "AbortError" });
  firstOwner.abort();
  expect(useCleanupStore.getState()).toMatchObject({ busy: false, preparedPlan: null });

  const nextOwner = new AbortController();
  const second = useCleanupStore.getState().prepareCleanup(cleanupPlan.actions, nextOwner.signal);
  const nextPlan = { ...cleanupPlan, planId: "cleanup:next" };
  next.resolve(nextPlan);
  await second;
  abandoned.resolve(cleanupPlan);
  await rejection;
  const discarded = () => ipc.invoke.mock.calls.filter(([command]) => command === "discard_cleanup_plan_cmd");
  expect(discarded()).toEqual([["discard_cleanup_plan_cmd", { planId: cleanupPlan.planId }]]);
  expect(useCleanupStore.getState().preparedPlan).toBe(nextPlan);

  nextOwner.abort();
  expect(useCleanupStore.getState()).toMatchObject({ busy: false, preparedPlan: null });
  expect(discarded()).toEqual([
    ["discard_cleanup_plan_cmd", { planId: cleanupPlan.planId }],
    ["discard_cleanup_plan_cmd", { planId: nextPlan.planId }],
  ]);
});

it("lets a replacement review use the slot released by Cancel", async () => {
  const released = deferred<void>();
  const nextPlan = { ...cleanupPlan, planId: "cleanup:replacement" };
  ipc.invoke.mockResolvedValueOnce(cleanupPlan).mockReturnValueOnce(released.promise).mockResolvedValueOnce(nextPlan);
  const owner = new AbortController();
  await useCleanupStore.getState().prepareCleanup(cleanupPlan.actions, owner.signal);
  owner.abort();
  const replacement = useCleanupStore.getState().prepareCleanup(cleanupPlan.actions);
  expect(ipc.invoke.mock.calls.map(([command]) => command)).toEqual(["prepare_cleanup_cmd", "discard_cleanup_plan_cmd"]);
  released.resolve();
  expect(await replacement).toBe(nextPlan);
  expect(ipc.invoke.mock.lastCall).toEqual(["prepare_cleanup_cmd", { actions: cleanupPlan.actions }]);
});

it("retains confirmed cleanup results after the preview owner leaves", async () => {
  const executing = deferred<{ path: string; status: "ok" }[]>();
  const owner = new AbortController();
  ipc.invoke.mockResolvedValueOnce(cleanupPlan).mockReturnValueOnce(executing.promise).mockResolvedValueOnce([]);
  const reviewed = await useCleanupStore.getState().prepareCleanup(cleanupPlan.actions, owner.signal);
  const execution = useCleanupStore.getState().executeCleanup(reviewed.planId);
  owner.abort();
  expect(useCleanupStore.getState()).toMatchObject({ busy: true, preparedPlan: null });
  executing.resolve([{ path: "a", status: "ok" }]);
  await execution;
  expect(useCleanupStore.getState().receipts.trash_large?.outcomes).toEqual([{ path: "a", status: "ok" }]);
});

it("executes frozen suggestion plans even if a later list is substituted", async () => {
  const selected = verifiedSuggestions("a", "b");
  ipc.invoke.mockResolvedValueOnce("review:one").mockResolvedValueOnce([plan("a"), plan("b")]);
  const reviewed = await useSuggestionsStore.getState().prepareSuggestions(selected, true);
  expect(ipc.invoke).toHaveBeenNthCalledWith(1, "begin_suggestion_preparation_cmd");
  expect(ipc.invoke).toHaveBeenNthCalledWith(2, "prepare_suggestions_cmd", {
    preparationId: "review:one",
    requests: selected.map(({ path, suggestedCategory }) => ({ path, suggestedCategory })),
    createRule: true,
  });
  expect(Object.isFrozen(reviewed)).toBe(true);
  expect(Object.isFrozen(reviewed[0].rule)).toBe(true);
  expect(distinctSuggestionRules(reviewed)).toHaveLength(1);
  useSuggestionsStore.setState({ suggestions: [suggestion("new-file")], lookup: { status: "failed", reason: "Lookup unavailable" } });
  ipc.invoke.mockResolvedValueOnce({ path: "a", status: "ok", message: null, dest: "root/Documents/a" }).mockResolvedValueOnce({ path: "b", status: "failed", message: "changed", dest: null });
  await useSuggestionsStore.getState().acceptAll(reviewed);
  expect(ipc.invoke).toHaveBeenCalledWith("accept_suggestion_cmd", { planId: "plan:a" });
  expect(ipc.invoke).toHaveBeenCalledWith("accept_suggestion_cmd", { planId: "plan:b" });
  expect(ipc.invoke).not.toHaveBeenCalledWith("accept_suggestion_cmd", { planId: "plan:new-file" });
  expect(useSuggestionsStore.getState().suggestions[0].path).toBe("new-file");
  expect(useSuggestionsStore.getState().results).toHaveLength(2);
});

it("cancels an in-flight suggestion batch before any plan IDs and preserves a later confirmed batch", async () => {
  const [firstCard, secondCard, nextCard] = verifiedSuggestions("a", "b", "next");
  const abandoned = deferred<SuggestionPlan[]>();
  ipc.invoke
    .mockResolvedValueOnce("review:abandoned").mockReturnValueOnce(abandoned.promise)
    .mockResolvedValueOnce(undefined)
    .mockResolvedValueOnce("review:next").mockResolvedValueOnce([plan("next")]);
  const firstOwner = new AbortController();
  const first = useSuggestionsStore.getState().prepareSuggestions([firstCard, secondCard], false, firstOwner.signal);
  const rejection = expect(first).rejects.toMatchObject({ name: "AbortError" });
  await vi.waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith("prepare_suggestions_cmd", expect.objectContaining({ preparationId: "review:abandoned" })));
  firstOwner.abort();
  expect(ipc.invoke).toHaveBeenCalledWith("cancel_suggestion_preparation_cmd", { preparationId: "review:abandoned" });
  const nextOwner = new AbortController();
  const reviewed = await useSuggestionsStore.getState().prepareSuggestions([nextCard], false, nextOwner.signal);

  abandoned.resolve([plan("a"), plan("b")]);
  await rejection;
  expect(useSuggestionsStore.getState()).toMatchObject({ busy: false, preparedPlans: reviewed, error: null });

  const executing = deferred<{ path: string; status: "ok"; dest: string; message: null }>();
  ipc.invoke.mockReturnValueOnce(executing.promise);
  const execution = useSuggestionsStore.getState().acceptAll(reviewed);
  nextOwner.abort();
  expect(useSuggestionsStore.getState()).toMatchObject({ busy: true, preparedPlans: null });
  executing.resolve({ path: "next", status: "ok", dest: "root/Documents/next", message: null });
  await execution;
  expect(useSuggestionsStore.getState().results?.[0]).toMatchObject({ path: "next", status: "ok" });
  expect(ipc.invoke.mock.calls.filter(([command]) => command === "cancel_suggestion_preparation_cmd")).toHaveLength(1);
});

it("releases a late reservation without starting the abandoned suggestion work", async () => {
  const selected = verifiedSuggestions("a");
  const reservation = deferred<string>();
  ipc.invoke.mockReturnValueOnce(reservation.promise).mockResolvedValueOnce(undefined);
  const owner = new AbortController();
  const first = useSuggestionsStore.getState().prepareSuggestions(selected, false, owner.signal);
  const rejection = expect(first).rejects.toMatchObject({ name: "AbortError" });
  owner.abort();
  expect(useSuggestionsStore.getState()).toMatchObject({ busy: false, preparedPlans: null });
  reservation.resolve("review:late");
  await rejection;
  expect(ipc.invoke.mock.calls).toEqual([
    ["begin_suggestion_preparation_cmd"],
    ["cancel_suggestion_preparation_cmd", { preparationId: "review:late" }],
  ]);
});

it("releases a prepared suggestion batch when its review is dismissed", async () => {
  const selected = verifiedSuggestions("a", "b");
  const owner = new AbortController();
  ipc.invoke.mockResolvedValueOnce("review:ready").mockResolvedValueOnce([plan("a"), plan("b")]).mockResolvedValueOnce(undefined);
  await useSuggestionsStore.getState().prepareSuggestions(selected, false, owner.signal);

  owner.abort();

  expect(ipc.invoke).toHaveBeenCalledWith("cancel_suggestion_preparation_cmd", { preparationId: "review:ready" });
  expect(useSuggestionsStore.getState()).toMatchObject({ busy: false, preparedPlans: null, error: null });
});

it("preserves failed cards for retry and reports a moved file with partial finalization", async () => {
  const selected = verifiedSuggestions("a", "b");
  ipc.invoke.mockResolvedValueOnce("review:partial").mockResolvedValueOnce([plan("a"), plan("b")]);
  const reviewed = await useSuggestionsStore.getState().prepareSuggestions(selected, false);
  ipc.invoke.mockRejectedValueOnce(new Error("scope changed")).mockResolvedValueOnce({ path: "b", status: "partial", message: "history failed", dest: "actual/b" });
  await useSuggestionsStore.getState().acceptAll(reviewed);
  expect(useSuggestionsStore.getState().suggestions.map((s) => s.path)).toEqual(["a"]);
  expect(useSuggestionsStore.getState().results?.[1]).toMatchObject({ status: "partial", dest: "actual/b" });
  ipc.invoke.mockResolvedValueOnce({ suggestions: [suggestion("a")], localAiStatus: null });
  await useSuggestionsStore.getState().loadSuggestions();
  expect(useSuggestionsStore.getState().results).toHaveLength(2);
});

it("rejects conflicting proposed global rules and leaves the selection intact", async () => {
  const selected = verifiedSuggestions("a", "b");
  ipc.invoke.mockResolvedValueOnce("review:conflict").mockRejectedValueOnce("Conflicting future rules for .xyz").mockResolvedValueOnce(undefined);
  await expect(useSuggestionsStore.getState().prepareSuggestions(selected, true)).rejects.toMatch("Conflicting future rules");
  expect(useSuggestionsStore.getState().preparedPlans).toBeNull();
  expect(useSuggestionsStore.getState().suggestions).toHaveLength(2);
});
