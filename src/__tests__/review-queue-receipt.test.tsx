import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import ReviewQueue from "../components/ReviewQueue";
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import { Results, type OperationResult } from "../components/BetaControls";
import { useReviewQueueStore } from "../store/useReviewQueueStore";

const fixture = vi.hoisted(() => ({
  invoke: vi.fn(), slots: [] as unknown[], cursor: 0, effects: [] as Array<() => void>,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: fixture.invoke }));
vi.mock("../store/useAppStore", () => ({
  useAppStore: { getState: () => ({ refreshActionViews: async () => {}, getPendingFiles: async () => {} }) },
  defaultRuleOptions: {},
}));
vi.mock("../store/useReviewQueueStore", async (original) => {
  const actual = await original<typeof import("../store/useReviewQueueStore")>();
  const store = actual.useReviewQueueStore;
  const snapshot = (select: (state: ReturnType<typeof store.getState>) => unknown) => select(store.getState());
  return { ...actual, useReviewQueueStore: Object.assign(snapshot, store) };
});
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string, options?: { defaultValue?: string }) => options?.defaultValue ?? key },
}));
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  function claim(initial: unknown) {
    const index = fixture.cursor++;
    if (!(index in fixture.slots)) fixture.slots[index] = initial;
    return index;
  }
  return {
    ...actual,
    useEffect: (effect: () => void) => { fixture.effects.push(effect); },
    useLayoutEffect: (effect: () => void) => { fixture.effects.push(effect); },
    useState: (initial: unknown) => {
      const index = claim(initial);
      return [
        fixture.slots[index],
        (next: unknown) => { fixture.slots[index] = typeof next === "function" ? next(fixture.slots[index]) : next; },
      ];
    },
    useRef: (initial: unknown) => fixture.slots[claim({ current: initial })],
  };
});

const source = "C:/Fixture/inbox/sample.txt";
const destination = "C:/Fixture/inbox/Sorted/sample_0.txt";
const sibling = "C:/Fixture/inbox/sibling.txt";
const preview = {
  id: "plan-fixture",
  entries: [
    { id: "approved", source, destination, action: "move", rule: "Docs", size: 6, warning: false, error: null },
    { id: "unchecked", source: sibling, destination: "C:/Fixture/inbox/Sorted/sibling.txt", action: "move", rule: "Docs", size: 11, warning: false, error: null },
  ],
};

interface Props extends Partial<ConfirmDialogProps> {
  children?: ReactNode;
  onClick?: (event: { currentTarget: HTMLButtonElement }) => void;
  onChange?: () => void;
  type?: string;
  checked?: boolean;
}

type Element = ReactElement<Props>;
function find(node: ReactNode, match: (element: Element) => boolean): Element | undefined {
  if (Array.isArray(node)) return node.map((child) => find(child, match)).find(Boolean);
  if (!isValidElement<Props>(node)) return;
  return match(node) ? node : find(node.props.children, match);
}

const paths = [source, sibling];
function renderQueue() {
  fixture.cursor = 0;
  return ReviewQueue();
}

beforeEach(() => {
  useReviewQueueStore.setState({ ...useReviewQueueStore.getInitialState(), requestedPaths: paths });
  fixture.invoke.mockReset();
  fixture.slots.length = 0;
  fixture.effects.length = 0;
});

async function chooseOne() {
  fixture.invoke.mockResolvedValueOnce(preview);
  renderQueue();
  fixture.effects.splice(0).forEach((effect) => effect());
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  const queue = renderQueue();
  const unchecked = find(queue, (element) => element.type === "label" &&
    !!find(element.props.children, (child) => child.props.children === sibling));
  find(unchecked?.props.children, (element) => element.type === "input")?.props.onChange?.();
  const apply = find(renderQueue(), (node) => node.type === "button" && node.props.children === "beta.apply");
  apply?.props.onClick?.({ currentTarget: {} as HTMLButtonElement });
  const confirm = find(renderQueue(), (node) => node.type === ConfirmDialog)?.props;
  if (!confirm?.onConfirm) throw new Error("The fixture selection did not open confirmation");
  return confirm;
}

it("sends a pair into the final confirm and applies only the selected cached entry", async () => {
  const confirm = await chooseOne();
  fixture.invoke.mockResolvedValueOnce([{ id: "approved", source, success: true, error: null, actualDestination: destination }]);
  await confirm.onConfirm!();

  expect(fixture.invoke).toHaveBeenLastCalledWith("apply_preview_cmd", { id: "plan-fixture", selected: ["approved"] });
  expect(confirm.count).toBe(1);
  expect(confirm.bytes).toBe(6);
  expect(confirm.preview).toEqual([{ source, destination }]);
});

function renderResult(fields: Partial<OperationResult>) {
  return renderToStaticMarkup(<Results results={[{ id: "approved", source, success: false, error: null, ...fields }]} />);
}

it("shows the native final destination when a reviewed move or rename succeeds", () => {
  const renameDestination = "C:/Fixture/inbox/renamed-sample_0.txt";
  for (const actualDestination of [destination, renameDestination]) {
    const markup = renderResult({ success: true, actualDestination });
    expect(markup).toContain("beta.done");
    expect(markup).toContain(source);
    expect(markup).toContain(actualDestination);
  }
});

it("shows a known completed native move with its destination and history warning", () => {
  const existingWarning = "Recovery record 47 is pending.";
  const markup = renderResult({ error: existingWarning, actualDestination: destination });
  expect(markup).toContain("beta.movedWithHistoryWarning");
  expect(markup).toContain(destination);
  expect(markup).toContain(existingWarning);
  expect(markup).not.toContain("beta.failed");
});

it("keeps the source and existing error on uncommitted failure, and accepts legacy success", () => {
  const failed = renderResult({ error: "The file changed after approval." });
  expect(failed).toContain("beta.failed");
  expect(failed).toContain(source);
  expect(failed).toContain("The file changed after approval.");
  expect(failed).not.toContain("beta.destination");
  expect(failed).not.toContain(destination);
  const legacy = renderResult({ success: true });
  expect(legacy).toContain("beta.done");
  expect(legacy).toContain(source);
  expect(legacy).not.toContain("beta.destination");
});
