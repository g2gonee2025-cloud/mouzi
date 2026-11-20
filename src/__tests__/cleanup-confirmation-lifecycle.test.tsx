import { isValidElement, type EffectCallback, type ReactElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import DuplicatesTab from "../components/cleanup/DuplicatesTab";
import EmptyDirsTab from "../components/cleanup/EmptyDirsTab";
import FileListTab from "../components/cleanup/FileListTab";
import ResultsPanel from "../components/cleanup/ResultsPanel";
import { useCleanupStore } from "../store/useCleanupStore";
import type { CleanupOutcome, CleanupRequest } from "../utils/cleanup";

const mock = vi.hoisted(() => ({
  invoke: vi.fn(),
  slots: [] as unknown[],
  cursor: 0,
  effects: new Map<number, { deps: readonly unknown[]; stop?: () => void }>(),
  layout: [] as Array<() => void>,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
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
      const slot = claim(initial);
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
  onChange?: () => void;
  results?: CleanupOutcome[];
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
    if (command === "get_cleanup_logs_cmd") return [];
    throw Error("Unexpected command " + command);
  });
});

afterEach(() => mock.effects.forEach(({ stop }) => stop?.()));

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
