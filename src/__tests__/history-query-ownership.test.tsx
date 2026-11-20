import { isValidElement, type ReactElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import HistoryView from "../components/HistoryView";
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import type { OrganizationLog } from "../store/useAppStore";

const mock = vi.hoisted(() => ({
  invoke: vi.fn(), listen: vi.fn(), slots: [] as unknown[], cursor: 0,
  effects: [] as Array<() => void | (() => void)>,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mock.listen }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../i18n", () => ({
  default: { t: (key: string, options?: { defaultValue?: string }) => options?.defaultValue ?? key },
}));
vi.mock("../store/useAppStore", () => ({
  useAppStore: { getState: () => ({ refreshActionViews: async () => {} }) },
  defaultRuleOptions: {},
}));
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  function claim(initial: unknown) {
    const index = mock.cursor++;
    if (!(index in mock.slots)) mock.slots[index] = initial;
    return index;
  }
  return {
    ...actual,
    useCallback: (fn: unknown) => fn,
    useRef: (current: unknown) => mock.slots[claim({ current })],
    useEffect: (effect: () => void | (() => void)) => { mock.effects.push(effect); },
    useState: (initial: unknown) => {
      const index = claim(initial);
      return [
        mock.slots[index],
        (next: unknown) => { mock.slots[index] = typeof next === "function" ? next(mock.slots[index]) : next; },
      ];
    },
  };
});

interface Props extends Partial<ConfirmDialogProps> {
  children?: ReactNode;
  role?: string;
  type?: string;
  disabled?: boolean;
  value?: string;
  onChange?: (event: { target: { value: string } }) => void;
  onClick?: (event: { currentTarget: HTMLButtonElement }) => void;
  onSubmit?: (event: { preventDefault: () => void }) => void;
}

type Element = ReactElement<Props>;
type Match = (node: Element) => boolean;
function text(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(text).join("");
  if (typeof node === "string") return node;
  return isValidElement<Props>(node) ? text(node.props.children) : "";
}
function find(node: ReactNode, match: Match): Element | undefined {
  if (Array.isArray(node)) return node.map((child) => find(child, match)).find(Boolean);
  if (!isValidElement<Props>(node)) return;
  return match(node) ? node : find(node.props.children, match);
}
function history() {
  mock.cursor = 0;
  return HistoryView();
}
function requireElement(match: Match, tree: ReactNode = history()) {
  const element = find(tree, match);
  if (!element) throw new Error("Expected History control was absent");
  return element;
}

const button = (label: string): Match => (node) => node.type === "button" && node.props.children === label;
const checkbox: Match = (node) => node.type === "input" && node.props.type === "checkbox";
const search: Match = (node) => node.type === "input" && node.props.type === "text";
const confirmation: Match = (node) => node.type === ConfirmDialog;
const click = (element: Element) => element.props.onClick!({ currentTarget: {} as HTMLButtonElement });
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const changeDraft = (value: string) => requireElement(search).props.onChange!({ target: { value } });
const submitFilters = () => requireElement((node) => node.type === "form").props.onSubmit!({ preventDefault: () => {} });
function apply(query: string) {
  changeDraft(query);
  submitFilters();
}
function changeDate(field: "from" | "to", value: string) {
  const label = requireElement((node) => node.type === "label" && text(node.props.children) === "beta." + field);
  requireElement((node) => node.type === "input" && node.props.type === "datetime-local", label)
    .props.onChange!({ target: { value } });
}
function chooseAll() {
  click(requireElement(button("beta.selectAll")));
  click(requireElement(button("beta.undoSelection")));
  return requireElement(confirmation).props;
}
function deferredRows() {
  let resolve!: (rows: OrganizationLog[]) => void;
  let reject!: (error: string) => void;
  const result = new Promise<OrganizationLog[]>((done, fail) => { resolve = done; reject = fail; });
  return { result, resolve, reject };
}

const row = (id: number, file: string, verified = true): OrganizationLog => ({
  id, timestamp: "2026-11-21T12:00:00Z", source_path: `C:/Fixture/${file}`,
  destination_path: `C:/Fixture/Sorted/${file}`, action: "move", file_name: file, file_type: "Fixture",
  undone: false, run_id: `run-${id}`, trigger: "manual", file_extension: "txt", file_size: 3,
  fingerprint: null, undo_verified: verified,
});
const first = row(41, "A.txt");
const second = row(42, "B.txt");
const legacy = {
  ...row(43, "B.txt", false),
  source_path: "C:/Fixture/Archive/B.txt",
  destination_path: "C:/Fixture/Archive/Sorted/B.txt",
};
let mutationCompleted: () => void;
let cleanup: Array<void | (() => void)>;

beforeEach(() => {
  mock.invoke.mockReset();
  mock.listen.mockImplementation(async (_name: string, handler: () => void) => {
    mutationCompleted = handler;
    return () => {};
  });
  mock.slots.length = 0;
  mock.effects.length = 0;
  cleanup = [];
});

afterEach(() => cleanup.forEach((unmount) => unmount?.()));

async function mount() {
  history();
  cleanup = mock.effects.splice(0).map((effect) => effect());
  await settle();
}

it.each([
  { name: "one local minute", from: "2026-11-21T10:15", to: "2026-11-21T10:15" },
  { name: "an end-only range at local midnight", from: "", to: "2026-11-21T23:59" },
  { name: "an end value carrying seconds", from: "", to: "2026-11-21T10:15:29.123" },
  { name: "a start-only range", from: "2026-11-22T00:00", to: "" },
])("sends precise History bounds for $name", async ({ from, to }) => {
  mock.invoke.mockResolvedValue([]);
  await mount();
  changeDate("from", from);
  changeDate("to", to);
  submitFilters();
  await settle();
  expect(mock.invoke).toHaveBeenLastCalledWith("history_cmd", {
    filter: {
      query: "", rule: "", extension: "", run_id: "",
      from: from ? new Date(from).toISOString() : "",
      to: to ? new Date(to.slice(0, 16) + ":59").toISOString().slice(0, -5) + ".999999999Z" : "",
    },
  });

  click(requireElement(button("beta.reset")));
  await settle();
  expect(mock.invoke).toHaveBeenLastCalledWith("history_cmd", {
    filter: { query: "", rule: "", extension: "", from: "", to: "", run_id: "" },
  });
});

it("rejects a reversed local minute range before querying History", async () => {
  mock.invoke.mockResolvedValue([]);
  await mount();
  changeDate("from", "2026-11-22T00:00");
  changeDate("to", "2026-11-21T23:59");
  submitFilters();
  expect(text(history())).toContain("beta.validation.date");
  expect(mock.invoke).toHaveBeenCalledTimes(1);
});

it("removes previous rows after a filter failure and retries the last applied filter without losing draft text", async () => {
  const pending = deferredRows();
  mock.invoke.mockResolvedValueOnce([first]).mockReturnValueOnce(pending.result);
  await mount();
  expect(text(history())).toContain(first.file_name);
  apply("B.txt");
  const whileLoading = history();
  pending.reject("Fixture filter read failed");
  await settle();

  const failed = history();
  expect(find(failed, (node) => node.props.role === "alert")?.props.children).toBe("Fixture filter read failed");
  expect(requireElement(button("beta.selectAll"), failed).props.disabled).toBe(true);
  expect(requireElement(button("beta.undoSelection"), failed).props.disabled).toBe(true);
  expect(find(failed, checkbox)).toBeUndefined();
  expect(text(failed)).not.toContain(first.file_name);
  expect(text(whileLoading)).not.toContain(first.file_name);
  expect(text(whileLoading)).toContain("common.loading");
  expect(text(failed)).not.toContain("beta.noHistory");

  changeDraft("Unsent search");
  mock.invoke.mockResolvedValueOnce([second, legacy]);
  click(requireElement(button("common.retry")));
  await settle();
  expect(mock.invoke).toHaveBeenLastCalledWith("history_cmd", {
    filter: { query: "B.txt", rule: "", extension: "", from: "", to: "", run_id: "" },
  });
  expect(requireElement(search).props.value).toBe("Unsent search");
  expect(text(history())).not.toContain(first.file_name);
  expect(text(history())).toContain(legacy.source_path);
  const confirm = chooseAll();
  expect(confirm.preview).toEqual([second.source_path]);
  confirm.onClose?.();
  expect(find(history(), confirmation)).toBeUndefined();
  expect(mock.invoke.mock.calls.every(([command]) => command !== "undo_selected_cmd")).toBe(true);
});

it("distinguishes a failed initial read from a verified empty result", async () => {
  mock.invoke.mockRejectedValueOnce("Initial history read failed").mockResolvedValueOnce([]);
  await mount();
  expect(text(history())).not.toContain("beta.noHistory");
  click(requireElement(button("common.retry")));
  await settle();
  expect(text(history())).toContain("beta.noHistory");
  expect(find(history(), button("common.retry"))).toBeUndefined();
});

it("invalidates unconfirmed consent when a new read starts and fences the delayed handler", async () => {
  const pending = deferredRows();
  mock.invoke.mockResolvedValueOnce([first]).mockReturnValueOnce(pending.result);
  await mount();
  const previous = chooseAll();
  mutationCompleted();
  const current = history();
  pending.resolve([second]);
  await settle();

  expect(find(current, confirmation)).toBeUndefined();
  await previous.onConfirm!();
  expect(mock.invoke.mock.calls.every(([command]) => command !== "undo_selected_cmd")).toBe(true);
});

it("preserves outcomes after an accepted restore even if the follow-up read fails", async () => {
  const outcome = [{ id: first.id, status: "ok", message: null, restoredTo: first.source_path }];
  let finishRestore!: (result: typeof outcome) => void;
  const pendingRestore = new Promise<typeof outcome>((resolve) => { finishRestore = resolve; });
  mock.invoke.mockResolvedValueOnce([first]).mockReturnValueOnce(pendingRestore)
    .mockRejectedValueOnce("Fixture mutation read failed").mockRejectedValueOnce("Fixture follow-up read failed");
  await mount();
  const confirm = chooseAll();
  const restoring = confirm.onConfirm!();
  mutationCompleted();
  await settle();
  expect(find(history(), confirmation)).toBeDefined();
  finishRestore(outcome);
  await restoring;
  confirm.onClose?.();

  expect(mock.invoke).toHaveBeenNthCalledWith(2, "undo_selected_cmd", { ids: [first.id] });
  expect(text(history())).toContain("undo.ok");
  expect(text(history())).toContain(first.source_path);
  expect(find(history(), checkbox)).toBeUndefined();
  expect(find(history(), button("common.retry"))).toBeDefined();
});

it("ignores a late failure after a newer read confirms the current query", async () => {
  const older = deferredRows();
  mock.invoke.mockResolvedValueOnce([first]).mockReturnValueOnce(older.result).mockResolvedValueOnce([second]);
  await mount();
  apply("B.txt");
  mutationCompleted();
  await settle();
  older.reject("Late history error");
  await settle();
  expect(text(history())).toContain(second.file_name);
  expect(text(history())).not.toContain("Late history error");
  expect(find(history(), button("common.retry"))).toBeUndefined();
});

it("keeps an Open folder failure separate from the confirmed History query", async () => {
  mock.invoke.mockResolvedValueOnce([first]).mockRejectedValueOnce("Folder is unavailable");
  await mount();
  click(requireElement(button("cleanup.openFolder")));
  await settle();
  expect(text(history())).toContain("Folder is unavailable");
  expect(find(history(), button("common.retry"))).toBeUndefined();
  expect(requireElement(checkbox).props.disabled).toBe(false);
});
