import { isValidElement, type EffectCallback, type ReactElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import DuplicatesTab from "../components/cleanup/DuplicatesTab";
import EmptyDirsTab from "../components/cleanup/EmptyDirsTab";
import FileListTab from "../components/cleanup/FileListTab";
import ResultsPanel from "../components/cleanup/ResultsPanel";
import Cleanup from "../pages/Cleanup";
import { useCleanupStore } from "../store/useCleanupStore";
import type { CleanupOutcome, CleanupRequest, DuplicateGroup } from "../utils/cleanup";

const mock = vi.hoisted(() => ({
  invoke: vi.fn(),
  slots: [] as unknown[],
  cursor: 0,
  effects: new Map<number, { deps: readonly unknown[]; stop?: () => void }>(),
  layout: [] as Array<() => void>,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({
  t: (key: string, options?: { path?: string }) => options?.path ? key + ": " + options.path : key,
}) }));
vi.mock("../components/cleanup/ConfirmDialog", () => ({ default: () => null }));
vi.mock("../components/cleanup/ResultsPanel", () => ({ default: () => null }));
vi.mock("../store/useCleanupStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useCleanupStore")>();
  const store = actual.useCleanupStore;
  const snapshot = (selector?: (state: ReturnType<typeof store.getState>) => unknown) =>
    selector ? selector(store.getState()) : store.getState();
  return { ...actual, useCleanupStore: Object.assign(snapshot, store) };
});
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  function claim(initial: unknown) {
    const slot = mock.cursor++;
    if (!(slot in mock.slots)) mock.slots[slot] = initial;
    return slot;
  }
  return {
    ...actual,
    useRef: (current: unknown) => mock.slots[claim({ current })],
    useState: (initial: unknown) => {
      const slot = claim(typeof initial === "function" ? initial() : initial);
      return [mock.slots[slot], (value: unknown) => {
        mock.slots[slot] = typeof value === "function" ? value(mock.slots[slot]) : value;
      }];
    },
    useLayoutEffect: (effect: EffectCallback, deps: readonly unknown[]) => {
      const slot = mock.cursor++;
      const existing = mock.effects.get(slot);
      if (existing && deps.every((value, index) => value === existing.deps[index])) return;
      mock.layout.push(() => {
        existing?.stop?.();
        mock.effects.set(slot, { deps, stop: effect() || undefined });
      });
    },
    useCallback: (fn: unknown) => fn,
    useEffect: () => {},
    useMemo: (fn: () => unknown) => fn(),
    useId: () => "fixture-threshold",
  };
});

type Tab = "duplicates" | "large" | "stale" | "empty";
type Props = Partial<ConfirmDialogProps> & {
  children?: ReactNode;
  role?: string;
  type?: string;
  disabled?: boolean;
  ref?: { current: HTMLButtonElement | null };
  onClick?: (event: { currentTarget: HTMLButtonElement }) => void;
  value?: string | number;
  onChange?: (event?: { target: { value: string } }) => void;
  results?: CleanupOutcome[];
  group?: DuplicateGroup;
  "aria-label"?: string;
  "aria-pressed"?: boolean;
};
type Element = ReactElement<Props>;
const findLabel: Record<Tab, string> = {
  duplicates: "cleanup.findDuplicates", large: "Find files", stale: "Find files", empty: "cleanup.findEmptyDirs",
};
const report = { path: "C:/Fixture/report.txt", size: 110 * 1_048_576, mtime: 1 };
const copy = { ...report, path: "C:/Fixture/copy.txt" };
const empty = "C:/Fixture/empty";
const findControl = {} as HTMLButtonElement;
const opener = {} as HTMLButtonElement;
const notice = "cleanup.reviewChanged";

function text(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(text).join("");
  if (typeof node === "string") return node;
  return isValidElement<Props>(node) ? text(node.props.children) : "";
}
function find(node: ReactNode, match: (element: Element) => boolean): Element | undefined {
  if (Array.isArray(node)) return node.map((child) => find(child, match)).find(Boolean);
  if (!isValidElement<Props>(node)) return;
  return match(node) ? node : find(node.props.children, match);
}
const button = (label: string) => (node: Element) => node.type === "button" && text(node.props.children) === label;
const dialog = (tree: ReactNode) => find(tree, (node) => node.type === ConfirmDialog)?.props;
const guidance = (tree: ReactNode) => find(tree, (node) => node.props.role === "status" && text(node) === notice);
const sent = () => mock.invoke.mock.calls.filter(([command]) => command === "execute_cleanup_cmd");
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function render(tab: Tab) {
  mock.cursor = 0;
  const tree = tab === "duplicates" ? DuplicatesTab()
    : tab === "empty" ? EmptyDirsTab()
    : FileListTab({ kind: tab, defaultThreshold: 100, findLabel: findLabel[tab], hint: "", emptyKey: "", operationKey: "review" });
  const findRef = find(tree, button(findLabel[tab]))?.props.ref;
  if (findRef) findRef.current = findControl;
  mock.layout.splice(0).forEach((effect) => effect());
  return tree;
}

async function open(tab: Tab) {
  if (tab !== "duplicates") {
    find(render(tab), (node) => node.type === "input" && node.props.type === "checkbox")!.props.onChange!();
  }
  find(render(tab), button("cleanup.confirm"))!.props.onClick!({ currentTarget: opener });
  await settle();
  const review = dialog(render(tab));
  expect(review).toBeDefined();
  return review!;
}

beforeEach(() => {
  mock.slots.length = 0;
  mock.effects.clear();
  mock.layout.length = 0;
  const store = useCleanupStore;
  store.setState(store.getInitialState(), true);
  store.setState({
    duplicates: [{ hash: "fixture-content", files: [report, copy] }],
    largeFiles: { minBytes: 100 * 1_048_576, files: [report] },
    staleFiles: { days: 100, files: [report] },
    emptyDirs: [empty],
  });
  mock.invoke.mockReset().mockImplementation(async (command: string, args: { actions?: CleanupRequest[] } = {}) => {
    if (command === "prepare_cleanup_cmd") {
      return {
        planId: "fixture-plan", expiresAt: "2027-01-01T00:00:00Z",
        actions: args.actions, totalBytes: report.size,
      };
    }
    if (command === "get_cleanup_history_page_cmd") return { entries: [], nextCursor: null };
    throw Error("Unexpected command " + command);
  });
});

afterEach(() => mock.effects.forEach(({ stop }) => stop?.()));

function duplicateCard(hash = "fixture-content") {
  const card = find(render("duplicates"), (node) => node.props.group?.hash === hash)!;
  const draw = card.type as (props: Props) => ReactNode;
  return draw(card.props);
}

function shownKeeper(hash = "fixture-content") {
  const keeper = find(duplicateCard(hash), (node) => node.type === "button" && node.props["aria-pressed"] === true);
  return keeper?.props["aria-label"]?.slice("cleanup.keepFile: ".length);
}

function chooseCopy(hash = "fixture-content", path = copy.path) {
  find(duplicateCard(hash), (node) => node.props["aria-label"] === "cleanup.keepFile: " + path)!
    .props.onClick!({ currentTarget: opener });
}

function closeTab() {
  mock.effects.forEach(({ stop }) => stop?.());
  mock.effects.clear();
  mock.slots.length = 0;
}

it("keeps the explicitly chosen copy when switching Cleanup tabs and canceling review", async () => {
  const shellSlots: unknown[] = [];
  function navigate(label: string, component: typeof DuplicatesTab | typeof FileListTab) {
    const tabSlots = mock.slots;
    mock.slots = shellSlots;
    try {
      mock.cursor = 0;
      find(Cleanup(), button(label))!.props.onClick!({ currentTarget: opener });
      mock.cursor = 0;
      expect(find(Cleanup(), (node) => node.type === component)).toBeDefined();
    } finally {
      mock.slots = tabSlots;
    }
    closeTab();
  }
  expect(shownKeeper()).toBe(report.path);
  chooseCopy();
  expect(shownKeeper()).toBe(copy.path);

  navigate("cleanup.tabs.large", FileListTab);
  render("large");
  navigate("cleanup.tabs.duplicates", DuplicatesTab);
  expect(shownKeeper()).toBe(copy.path);
  for (let attempt = 0; attempt < 2; attempt++) {
    const review = await open("duplicates");
    expect(mock.invoke.mock.lastCall).toEqual(["prepare_cleanup_cmd", {
      actions: [{ kind: "trash_duplicate", path: report.path, keepPath: copy.path }],
    }]);
    expect(review.preview).toEqual([report.path]);
    review.onClose!();
    expect(dialog(render("duplicates"))).toBeUndefined();
    expect(shownKeeper()).toBe(copy.path);
  }
  expect(sent()).toEqual([]);
});

it("retains only a valid explicit keeper on a repeat search and clears it after invalidation", async () => {
  const hash = "fixture-content";
  const member = (name: string) => ({ ...report, path: "C:/Fixture/" + name });
  const extra = member("extra.txt");
  const third = member("third.txt");
  const otherGroup = { hash: "new-content", files: [third, member("fourth.txt")] };
  chooseCopy();
  mock.invoke.mockResolvedValueOnce([{ hash, files: [report, extra, copy] }, otherGroup]);
  await useCleanupStore.getState().findDuplicates();
  expect(shownKeeper()).toBe(copy.path);
  expect(shownKeeper("new-content")).toBe(third.path);

  const displacedGroup = { hash: "new-content", files: [third, copy] };
  mock.invoke.mockResolvedValueOnce([{ hash, files: [extra, report] }, displacedGroup]);
  await useCleanupStore.getState().findDuplicates();
  expect(shownKeeper()).toBe(extra.path);
  expect(shownKeeper("new-content")).toBe(third.path);
  chooseCopy(hash, report.path);
  useCleanupStore.getState().invalidateFileCandidates();
  expect(useCleanupStore.getState().duplicates).toBe(null);
  mock.invoke.mockResolvedValueOnce([{ hash, files: [extra, report] }]);
  await useCleanupStore.getState().findDuplicates();
  expect(shownKeeper()).toBe(extra.path);
  expect(sent()).toEqual([]);
});

it("prepares ordered victims from the current choices for more than one group", async () => {
  const third = { ...report, path: "C:/Fixture/third.txt" };
  const smaller = { path: "C:/Fixture/smaller.txt", size: 25, mtime: 1 };
  const smallerCopy = { ...smaller, path: "C:/Fixture/smaller-copy.txt" };
  mock.invoke.mockResolvedValueOnce([
    { hash: "smaller", files: [smaller, smallerCopy] },
    { hash: "fixture-content", files: [report, copy, third] },
  ]);
  await useCleanupStore.getState().findDuplicates();
  const oldConfirm = find(render("duplicates"), button("cleanup.confirm"))!;
  chooseCopy();
  oldConfirm.props.onClick!({ currentTarget: opener });
  await settle();
  expect(mock.invoke.mock.calls.map(([command]) => command)).toEqual(["find_duplicates_cmd"]);

  const review = await open("duplicates");
  expect(mock.invoke.mock.lastCall).toEqual(["prepare_cleanup_cmd", { actions: [
    { kind: "trash_duplicate", path: report.path, keepPath: copy.path },
    { kind: "trash_duplicate", path: third.path, keepPath: copy.path },
    { kind: "trash_duplicate", path: smallerCopy.path, keepPath: smaller.path },
  ] }]);
  expect(review.preview).toEqual([report.path, third.path, smallerCopy.path]);
  review.onClose!();
  expect(sent()).toEqual([]);
});


it.each<Tab>(["duplicates", "large", "stale", "empty"])("retires an invalidated %s dialog and fences its stale Confirm callback", async (tab) => {
  const previous = await open(tab);
  useCleanupStore.getState().invalidateCandidates();
  useCleanupStore.getState().invalidateCandidates();
  const retired = render(tab);

  expect(dialog(retired)).toBeUndefined();
  expect(guidance(retired)).toBeDefined();
  expect(find(retired, button(findLabel[tab]))!.props.disabled).toBe(false);
  expect(previous.focusFallback?.()).toBe(findControl);
  await previous.onConfirm?.();
  expect(sent()).toEqual([]);
  expect(guidance(render(tab))).toBeDefined();
});

it("keeps the exact review across a published scan and distinguishes Cancel from invalidation", async () => {
  const review = await open("duplicates");
  const plan = useCleanupStore.getState().preparedPlan;
  useCleanupStore.getState().invalidateFileCandidates();
  expect(dialog(render("duplicates"))?.onConfirm).toBe(review.onConfirm);
  expect(useCleanupStore.getState().preparedPlan).toBe(plan);
  review.onClose?.();
  expect(dialog(render("duplicates"))).toBeUndefined();
  expect(useCleanupStore.getState().preparedPlan).toBeNull();
  expect(guidance(render("duplicates"))).toBeUndefined();
  expect(sent()).toEqual([]);
});

it("blocks a captured Confirm even before the invalidating change is rendered", async () => {
  const previous = await open("large");
  useCleanupStore.getState().invalidateCandidates();
  await expect(previous.onConfirm?.()).resolves.toBeUndefined();
  expect(sent()).toEqual([]);
  expect(dialog(render("large"))).toBeUndefined();
  expect(guidance(render("large"))).toBeDefined();
});

it("preserves a dispatched confirmation through overlapping events and shows its ordinary receipt", async () => {
  const review = await open("large");
  const outcomes: CleanupOutcome[] = [{ path: report.path, status: "failed", message: "Retained fixture" }];
  let finish!: (outcomes: CleanupOutcome[]) => void;
  const result = new Promise<CleanupOutcome[]>((resolve) => { finish = resolve; });
  mock.invoke.mockReturnValueOnce(result);
  const execution = review.onConfirm?.();
  expect(useCleanupStore.getState().preparedPlan).toBeNull();
  expect(sent()).toEqual([["execute_cleanup_cmd", { planId: "fixture-plan" }]]);
  useCleanupStore.getState().invalidateCandidates();
  useCleanupStore.getState().invalidateFileCandidates();
  expect(dialog(render("large"))?.onConfirm).toBe(review.onConfirm);
  expect(guidance(render("large"))).toBeUndefined();

  finish(outcomes);
  await execution;
  review.onClose?.();
  const receipt = render("large");
  expect(dialog(receipt)).toBeUndefined();
  expect(guidance(receipt)).toBeUndefined();
  expect(find(receipt, (node) => node.type === ResultsPanel)?.props.results).toEqual(outcomes);
  expect(useCleanupStore.getState().results).toEqual(outcomes);
});

type FileTab = "large" | "stale";
const mib = 1_048_576;
const findCommand = {
  large: "find_large_files_cmd",
  stale: "find_stale_files_cmd",
};
const finderArguments = {
  large: (value: number) => ({ minBytes: value }),
  stale: (value: number) => ({ days: value }),
};

function threshold(tree: ReactNode) {
  return find(tree, (node) => node.type === "input" && node.props.type === "number")!.props;
}

function replaceThreshold(tab: FileTab, value: string) {
  threshold(render(tab)).onChange!({ target: { value } });
  return render(tab);
}

function findFiles(tree: ReactNode) {
  return find(tree, button("Find files"))!.props;
}

it.each([
  { tab: "large", replacement: "1.5", expected: 1.5 * mib },
  { tab: "stale", replacement: "25", expected: 25 },
] as const)("clears and replaces the $tab threshold without submitting a default", async ({ tab, replacement, expected }) => {
  const original = render(tab);
  find(original, (node) => node.type === "input" && node.props.type === "checkbox")!.props.onChange!();
  expect(find(render(tab), button("cleanup.confirm"))?.props.disabled).toBe(false);

  const blank = replaceThreshold(tab, "");
  expect(String(threshold(blank).value)).toBe("");
  expect(findFiles(blank).disabled).toBe(true);
  expect(find(blank, button("cleanup.confirm"))).toBeUndefined();
  findFiles(blank).onClick!({ currentTarget: findControl });
  expect(mock.invoke).not.toHaveBeenCalled();

  const edited = replaceThreshold(tab, replacement);
  expect(String(threshold(edited).value)).toBe(replacement);
  expect(findFiles(edited).disabled).toBe(false);
  mock.invoke.mockResolvedValueOnce([]);
  findFiles(edited).onClick!({ currentTarget: findControl });
  await settle();
  expect(mock.invoke).toHaveBeenCalledExactlyOnceWith(findCommand[tab], finderArguments[tab](expected));
});

it.each([
  ["large", "0.5"],
  ["large", "1.1"],
  ["large", "1.0000000000000001"],
  ["large", "8589934592"],
  ["stale", "0"],
  ["stale", "2.5"],
  ["stale", "1.00000000000000001"],
  ["stale", "106751991167301"],
] as const)("blocks the %s input %s before IPC", (tab, raw) => {
  const tree = replaceThreshold(tab, raw);
  expect(String(threshold(tree).value)).toBe(raw);
  expect(findFiles(tree).disabled).toBe(true);
  expect(find(tree, button("cleanup.confirm"))).toBeUndefined();
  findFiles(tree).onClick!({ currentTarget: findControl });
  expect(mock.invoke).not.toHaveBeenCalled();
});

it.each([
  ["large", "1.00000095367431640625", 1_048_577, "1.00000095367431640625"],
  ["large", "15e-1", 1_572_864, "1.5"],
  ["large", "8589934591.99999904632568359375", Number.MAX_SAFE_INTEGER, "8589934591.99999904632568359375"],
  ["stale", "106751991167300", 106751991167300, "106751991167300"],
] as const)("retries an exact %s threshold %s across a tab remount", async (tab, raw, payload, savedText) => {
  const edited = replaceThreshold(tab, raw);
  expect(findFiles(edited).disabled).toBe(false);
  mock.invoke.mockRejectedValueOnce("Finder unavailable");
  findFiles(edited).onClick!({ currentTarget: findControl });
  await settle();

  mock.effects.forEach(({ stop }) => stop?.());
  mock.effects.clear();
  mock.slots.length = 0;
  const remounted = render(tab);
  expect(String(threshold(remounted).value)).toBe(savedText);
  expect(findFiles(remounted).disabled).toBe(false);
  expect(find(remounted, button("cleanup.confirm"))).toBeUndefined();

  mock.invoke.mockResolvedValueOnce([]);
  findFiles(remounted).onClick!({ currentTarget: findControl });
  await settle();
  const call = [findCommand[tab], finderArguments[tab](payload)];
  expect(mock.invoke.mock.calls).toEqual([call, call]);
});
