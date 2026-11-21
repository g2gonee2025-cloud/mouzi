import { isValidElement, type ReactElement, type ReactNode } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import Suggestions from "../pages/Suggestions";
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import { useSuggestionsStore } from "../store/useSuggestionsStore";

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), stateSlots: null as unknown[] | null, cursor: 0 }));
vi.mock("@tauri-apps/api/core", () => ipc);
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: () => {},
  useLayoutEffect: () => {},
  useRef: (current: unknown) => ({ current }),
  useState: (value: unknown) => {
    const slots = ipc.stateSlots;
    if (!slots) return [value, vi.fn()];
    const slot = ipc.cursor++;
    if (!(slot in slots)) slots[slot] = value;
    return [slots[slot], (next: unknown) => {
      slots[slot] = typeof next === "function" ? next(slots[slot]) : next;
    }];
  },
}));
vi.mock("../store/useSuggestionsStore", async (importOriginal) => {
  const { useSuggestionsStore: store, ...exports } = await importOriginal<typeof import("../store/useSuggestionsStore")>();
  return { ...exports, useSuggestionsStore: Object.assign(store.getState, store) };
});

const invoice = {
  path: "C:/Fixture/invoice.unrecognized",
  filename: "invoice.unrecognized",
  suggestedCategory: "Documents",
  currentCategory: "Other",
  confidence: 0.65,
  source: "heuristic" as const,
  version: { identity: "file-A", size: "14", mtimeNs: "1000" },
};
const replacement = { ...invoice, version: { identity: "file-B", size: "36", mtimeNs: "2000" } };
const receipt = {
  ...invoice,
  path: "C:/Fixture/receipt.unrecognized",
  filename: "receipt.unrecognized",
  version: { identity: "file-C", size: "8", mtimeNs: "3000" },
};

interface Props extends Partial<ConfirmDialogProps> {
  children?: ReactNode;
  role?: string;
  tabIndex?: number;
  "aria-label"?: string;
  "aria-live"?: string;
  "aria-atomic"?: string;
  "aria-relevant"?: string;
  onClick?: (event?: { currentTarget: HTMLButtonElement }) => Promise<void> | void;
}
function text(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(text).join("");
  if (typeof node === "string") return node;
  return isValidElement<Props>(node) ? text(node.props.children) : "";
}
function find(node: ReactNode, match: (element: ReactElement<Props>) => boolean): ReactElement<Props> | undefined {
  if (Array.isArray(node)) return node.map((child) => find(child, match)).find(Boolean);
  if (!isValidElement<Props>(node)) return;
  return match(node) ? node : find(node.props.children, match);
}
function dismissAction(tree: ReactNode, bulk = false) {
  const label = bulk ? "suggestions.dismissAll" : "suggestions.dismiss";
  return find(tree, (button) => button.type === "button"
    && (button.props["aria-label"] === label || text(button) === label))!.props.onClick!;
}
const dismissCommand = "dismiss_suggestion_cmd";
const refreshCommand = "get_suggestions_with_status_cmd";
const dismissal = ({ path, version }: typeof invoice) => [dismissCommand, { path, version }];

function showVerified(...suggestions: typeof invoice[]) {
  useSuggestionsStore.setState({ suggestions, lookup: { status: "ready" } });
}

beforeEach(() => {
  ipc.stateSlots = null;
  ipc.invoke.mockReset();
  useSuggestionsStore.setState(useSuggestionsStore.getInitialState(), true);
});

it("uses the same polite receipt region and adds failures after confirmation closes", async () => {
  ipc.stateSlots = [];
  showVerified(invoice);
  const render = () => {
    ipc.cursor = 0;
    return Suggestions();
  };
  const log = (tree: ReactNode) => find(tree, (item) => item.props.role === "log");
  const semantics = {
    "aria-label": "suggestions.resultsTitle",
    "aria-live": "polite",
    "aria-atomic": "false",
    "aria-relevant": "additions",
  };
  const initial = render();
  expect.soft(log(initial)?.props).toMatchObject(semantics);
  expect(log(initial)?.props.tabIndex).toBeUndefined();

  ipc.invoke.mockResolvedValueOnce("fixture-preparation").mockResolvedValueOnce([{
    planId: "fixture-plan", path: invoice.path, destination: "C:/Filed/invoice.unrecognized",
    category: invoice.suggestedCategory, size: 14, rule: null,
  }]);
  find(initial, (item) => item.props["aria-label"] === "suggestions.accept")!.props.onClick!({
    currentTarget: {} as HTMLButtonElement,
  });
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  const originalDialog = find(render(), (item) => item.type === ConfirmDialog)!.props;
  const reason = "Move refused before publication";
  ipc.invoke.mockRejectedValueOnce(reason);
  await originalDialog.onConfirm!();
  expect(useSuggestionsStore.getState().results?.[0].message).toBe(reason);
  expect.soft(text(log(render()))).not.toContain(reason);
  originalDialog.onClose!();

  const after = render();
  expect.soft(log(after)?.props).toMatchObject({ ...semantics, tabIndex: 0 });
  expect.soft(text(log(after))).toContain("suggestions.failed");
  expect.soft(text(log(after))).toContain(reason);
  expect(text(after).split(reason)).toHaveLength(2);
  expect(find(after, (item) => item.props["aria-label"] === "suggestions.accept")).toBeDefined();
  expect(find(after, (item) => item.type === "h2" && text(item) === "suggestions.resultsTitle")?.props.tabIndex).toBe(-1);
});

it("submits the listed version for normal single dismissal and removes its row", async () => {
  showVerified(invoice);
  ipc.invoke.mockResolvedValueOnce(true);
  await dismissAction(Suggestions())();
  expect(ipc.invoke.mock.calls).toEqual([dismissal(invoice)]);
  expect(useSuggestionsStore.getState().suggestions).toEqual([]);
});

it("refreshes a rejected single dismissal and keeps the replacement discoverable", async () => {
  showVerified(invoice);
  ipc.invoke.mockResolvedValueOnce(false).mockResolvedValueOnce({ suggestions: [replacement], localAiStatus: null });
  await dismissAction(Suggestions())();
  expect(useSuggestionsStore.getState().suggestions).toEqual([replacement]);
  expect(ipc.invoke.mock.calls).toEqual([dismissal(invoice), [refreshCommand, { limit: 100 }]]);
  expect(find(Suggestions(), (element) => element.props.role === "status" && text(element) === "suggestions.dismissChanged")).toBeDefined();
});

it("continues Dismiss All after a stale card and targets only the cards shown at click", async () => {
  showVerified(invoice, receipt);
  const click = dismissAction(Suggestions(), true);
  let finish!: (dismissed: boolean) => void;
  ipc.invoke.mockReturnValueOnce(new Promise<boolean>((resolve) => { finish = resolve; }))
    .mockResolvedValueOnce(true).mockResolvedValueOnce({ suggestions: [replacement], localAiStatus: null });
  const dismissalBatch = click();
  useSuggestionsStore.setState({ suggestions: [replacement, receipt] });
  finish(false);
  await dismissalBatch;
  expect(ipc.invoke.mock.calls).toEqual([dismissal(invoice), dismissal(receipt), [refreshCommand, { limit: 100 }]]);
  expect(useSuggestionsStore.getState().suggestions).toEqual([replacement]);
  expect(find(Suggestions(), (element) => element.props.role === "status" && text(element) === "suggestions.dismissChanged")).toBeDefined();
});

it("clears an automatic-refresh dismissal notice on the next ordinary Refresh", async () => {
  showVerified(invoice);
  const latest = { suggestions: [replacement], localAiStatus: null };
  ipc.invoke.mockResolvedValueOnce(false).mockResolvedValueOnce(latest).mockResolvedValueOnce(latest);
  await dismissAction(Suggestions())();
  const hasNotice = () => find(Suggestions(), (element) => element.props.role === "status"
    && text(element) === "suggestions.dismissChanged");
  expect(hasNotice()).toBeDefined();

  const refresh = find(Suggestions(), (element) => element.type === "button" && text(element) === "suggestions.refresh");
  await refresh!.props.onClick!();
  expect(hasNotice()).toBeUndefined();
});

it("preserves a later bulk failure and lets the user refresh after a stale dismissal", async () => {
  showVerified(invoice, receipt);
  ipc.invoke.mockResolvedValueOnce(false).mockRejectedValueOnce("Could not save dismissal");
  await dismissAction(Suggestions(), true)();

  const tree = Suggestions();
  expect(text(tree)).toContain("Could not save dismissal");
  expect(text(tree)).toContain("suggestions.dismissChanged");
  expect(useSuggestionsStore.getState().suggestions).toEqual([receipt]);
  expect(ipc.invoke.mock.calls).toEqual([dismissal(invoice), dismissal(receipt)]);
  const refresh = find(tree, (element) => element.type === "button" && text(element) === "suggestions.refresh");
  ipc.invoke.mockResolvedValueOnce({ suggestions: [replacement, receipt], localAiStatus: null });
  await refresh!.props.onClick!();
  expect(useSuggestionsStore.getState().suggestions).toEqual([replacement, receipt]);
  expect(text(Suggestions())).not.toContain("Could not save dismissal");
  expect(text(Suggestions())).not.toContain("suggestions.dismissChanged");
});

it.each([
  ["suggestions.accept", "pending"],
  ["suggestions.acceptAll", "pending"],
  ["suggestions.dismiss", "pending"],
  ["suggestions.dismissAll", "pending"],
  ["suggestions.accept", "failed"],
  ["suggestions.acceptAll", "failed"],
  ["suggestions.dismiss", "failed"],
  ["suggestions.dismissAll", "failed"],
] as const)("blocks the captured %s callback while the lookup is %s", async (label, phase) => {
  const store = useSuggestionsStore.getState();
  const response = { suggestions: [invoice, receipt], localAiStatus: null };
  ipc.invoke.mockResolvedValueOnce(response);
  await store.loadSuggestions();
  const captured = find(Suggestions(), (node) => node.type === "button"
    && (node.props["aria-label"] === label || text(node) === label))!.props.onClick!;

  let finish!: (result: typeof response) => void;
  if (phase === "failed") ipc.invoke.mockRejectedValueOnce("Directory lookup failed");
  else ipc.invoke.mockReturnValueOnce(new Promise<typeof response>((resolve) => { finish = resolve; }));
  const refreshed = store.loadSuggestions();
  if (phase === "failed") await refreshed;
  ipc.invoke.mockImplementation(async (command: string) => {
    if (command === "begin_suggestion_preparation_cmd") return "fixture-preparation";
    if (command === "prepare_suggestions_cmd") return [];
    if (command === dismissCommand) return true;
    throw new Error("Unexpected command " + command);
  });
  const priorCalls = ipc.invoke.mock.calls.length;
  await captured({ currentTarget: {} as HTMLButtonElement });
  if (phase === "pending") {
    finish(response);
    await refreshed;
  }
  expect(ipc.invoke.mock.calls.slice(priorCalls)).toEqual([]);
});
