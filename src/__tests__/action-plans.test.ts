import { beforeEach, expect, it, vi } from "vitest";
const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ipc);
import { useCleanupStore } from "../store/useCleanupStore";
import { useSuggestionsStore, distinctSuggestionRules, type Suggestion, type SuggestionPlan } from "../store/useSuggestionsStore";
import type { CleanupPlan } from "../utils/cleanup";

const suggestion = (path: string): Suggestion => ({ path, filename: "report.xyz", suggestedCategory: "Documents", currentCategory: "Other", confidence: 0.5, source: "heuristic" });
const plan = (path: string, category = "Documents"): SuggestionPlan => ({ path, planId: `plan:${path}`, category, destination: `root/${category}/report.xyz`, rule: { extension: "xyz", destination: category, scope: "All eligible roots", priority: 10, disposition: "create", message: null } });
const cleanupPlan: CleanupPlan = { planId: "cleanup:reviewed", expiresAt: "2026-10-03T12:00:00Z", actions: [{ kind: "trash_large", path: "a", minBytes: 100 }, { kind: "trash_large", path: "b", minBytes: 100 }], totalBytes: 200 };
const row = (path: string) => ({ path, size: 100, mtime: 1 });
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; }

beforeEach(() => {
  ipc.invoke.mockReset();
  useCleanupStore.setState({ duplicates: null, largeFiles: null, staleFiles: null, emptyDirs: null, loading: false, busy: false, preparedPlan: null, results: null, error: null, history: null, historyLoading: false, historyError: null });
  useSuggestionsStore.setState({ suggestions: [], loading: false, busy: false, error: null, preparedPlans: null, results: null });
});

it("previews cleanup criteria and executes only the reviewed backend token", async () => {
  useCleanupStore.setState({ largeFiles: [row("a"), row("b")] });
  ipc.invoke.mockResolvedValueOnce(cleanupPlan);
  const reviewed = await useCleanupStore.getState().prepareCleanup(cleanupPlan.actions);
  expect(ipc.invoke).toHaveBeenCalledWith("prepare_cleanup_cmd", { actions: cleanupPlan.actions });
  expect(ipc.invoke).not.toHaveBeenCalledWith("execute_cleanup_cmd", expect.anything());
  ipc.invoke.mockResolvedValueOnce([{ path: "a", status: "ok", message: "Recovery unverified" }, { path: "b", status: "failed", message: "File changed" }]).mockResolvedValueOnce([]);
  await useCleanupStore.getState().executeCleanup(reviewed.planId);
  expect(ipc.invoke).toHaveBeenCalledWith("execute_cleanup_cmd", { planId: "cleanup:reviewed" });
  expect(useCleanupStore.getState().largeFiles?.map((file) => file.path)).toEqual(["b"]);
  expect(useCleanupStore.getState().results?.[1].message).toBe("File changed");
  useCleanupStore.getState().invalidateCandidates();
  expect(useCleanupStore.getState().results).toHaveLength(2);
});

it("discards a late finder and refuses a preview invalidated during preparation", async () => {
  const older = deferred<ReturnType<typeof row>[]>();
  ipc.invoke.mockReturnValueOnce(older.promise).mockResolvedValueOnce([row("current")]);
  const first = useCleanupStore.getState().findLargeFiles(10);
  await useCleanupStore.getState().findLargeFiles(20);
  older.resolve([row("old")]); await first;
  expect(useCleanupStore.getState().largeFiles?.[0].path).toBe("current");
  const pending = deferred<CleanupPlan>(); ipc.invoke.mockReturnValueOnce(pending.promise);
  const preparing = useCleanupStore.getState().prepareCleanup(cleanupPlan.actions);
  const rejection = expect(preparing).rejects.toThrow("candidates changed");
  useCleanupStore.getState().invalidateCandidates(); pending.resolve(cleanupPlan);
  await rejection;
  expect(useCleanupStore.getState().preparedPlan).toBeNull();
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
  abandoned.resolve(cleanupPlan);
  await rejection;
  expect(useCleanupStore.getState()).toMatchObject({ busy: true, preparedPlan: null, error: null });
  const nextPlan = { ...cleanupPlan, planId: "cleanup:next" };
  next.resolve(nextPlan);
  await second;
  expect(useCleanupStore.getState().preparedPlan).toBe(nextPlan);

  nextOwner.abort();
  expect(useCleanupStore.getState()).toMatchObject({ busy: false, preparedPlan: null });
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
  expect(useCleanupStore.getState().results).toEqual([{ path: "a", status: "ok" }]);
});

it("executes frozen suggestion plans even if a later list is substituted", async () => {
  const selected = [suggestion("a"), suggestion("b")];
  useSuggestionsStore.setState({ suggestions: selected });
  ipc.invoke.mockResolvedValueOnce(plan("a")).mockResolvedValueOnce(plan("b"));
  const reviewed = await useSuggestionsStore.getState().prepareSuggestions(selected, true);
  expect(Object.isFrozen(reviewed)).toBe(true);
  expect(Object.isFrozen(reviewed[0].rule)).toBe(true);
  expect(distinctSuggestionRules(reviewed)).toHaveLength(1);
  useSuggestionsStore.setState({ suggestions: [suggestion("new-file")] });
  ipc.invoke.mockResolvedValueOnce({ path: "a", status: "ok", message: null, dest: "root/Documents/a" }).mockResolvedValueOnce({ path: "b", status: "failed", message: "changed", dest: null });
  await useSuggestionsStore.getState().acceptAll(reviewed);
  expect(ipc.invoke).toHaveBeenCalledWith("accept_suggestion_cmd", { planId: "plan:a" });
  expect(ipc.invoke).toHaveBeenCalledWith("accept_suggestion_cmd", { planId: "plan:b" });
  expect(ipc.invoke).not.toHaveBeenCalledWith("accept_suggestion_cmd", { planId: "plan:new-file" });
  expect(useSuggestionsStore.getState().suggestions[0].path).toBe("new-file");
  expect(useSuggestionsStore.getState().results).toHaveLength(2);
});

it("stops preparing an abandoned suggestion batch and preserves a later confirmed batch", async () => {
  const abandoned = deferred<SuggestionPlan>();
  ipc.invoke.mockReturnValueOnce(abandoned.promise).mockResolvedValueOnce(plan("next"));
  const firstOwner = new AbortController();
  const first = useSuggestionsStore.getState().prepareSuggestions([suggestion("a"), suggestion("b")], false, firstOwner.signal);
  const rejection = expect(first).rejects.toMatchObject({ name: "AbortError" });
  firstOwner.abort();
  const nextOwner = new AbortController();
  const reviewed = await useSuggestionsStore.getState().prepareSuggestions([suggestion("next")], false, nextOwner.signal);

  abandoned.resolve(plan("a"));
  await rejection;
  expect(ipc.invoke).toHaveBeenCalledTimes(2);
  expect(useSuggestionsStore.getState()).toMatchObject({ busy: false, preparedPlans: reviewed, error: null });

  const executing = deferred<{ path: string; status: "ok"; dest: string; message: null }>();
  ipc.invoke.mockReturnValueOnce(executing.promise);
  const execution = useSuggestionsStore.getState().acceptAll(reviewed);
  nextOwner.abort();
  expect(useSuggestionsStore.getState()).toMatchObject({ busy: true, preparedPlans: null });
  executing.resolve({ path: "next", status: "ok", dest: "root/Documents/next", message: null });
  await execution;
  expect(useSuggestionsStore.getState().results?.[0]).toMatchObject({ path: "next", status: "ok" });
});

it("preserves failed cards for retry and reports a moved file with partial finalization", async () => {
  const selected = [suggestion("a"), suggestion("b")]; useSuggestionsStore.setState({ suggestions: selected });
  ipc.invoke.mockResolvedValueOnce(plan("a")).mockResolvedValueOnce(plan("b"));
  const reviewed = await useSuggestionsStore.getState().prepareSuggestions(selected, false);
  ipc.invoke.mockRejectedValueOnce(new Error("scope changed")).mockResolvedValueOnce({ path: "b", status: "partial", message: "history failed", dest: "actual/b" });
  await useSuggestionsStore.getState().acceptAll(reviewed);
  expect(useSuggestionsStore.getState().suggestions.map((s) => s.path)).toEqual(["a"]);
  expect(useSuggestionsStore.getState().results?.[1]).toMatchObject({ status: "partial", dest: "actual/b" });
  ipc.invoke.mockResolvedValueOnce([suggestion("a")]); await useSuggestionsStore.getState().loadSuggestions();
  expect(useSuggestionsStore.getState().results).toHaveLength(2);
});

it("rejects conflicting proposed global rules and leaves the selection intact", async () => {
  const selected = [suggestion("a"), suggestion("b")]; useSuggestionsStore.setState({ suggestions: selected });
  ipc.invoke.mockResolvedValueOnce(plan("a")).mockResolvedValueOnce(plan("b", "Images"));
  await expect(useSuggestionsStore.getState().prepareSuggestions(selected, true)).rejects.toThrow("Conflicting future rules");
  expect(useSuggestionsStore.getState().preparedPlans).toBeNull();
  expect(useSuggestionsStore.getState().suggestions).toHaveLength(2);
});
