import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Children, isValidElement, type EffectCallback, type ReactNode, type ReactElement } from "react";
import ReviewQueue from "../components/ReviewQueue";
import ConfirmDialog from "../components/cleanup/ConfirmDialog";
import { Results, type OperationResult } from "../components/BetaControls";

const harness = vi.hoisted(() => ({
  slots: [] as unknown[],
  cursor: 0,
  domains: undefined as readonly unknown[] | undefined,
  pending: undefined as EffectCallback | undefined,
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: harness.invoke }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: { count?: number }) => options?.count === undefined ? key : `${key}:${options.count}`,
  }),
}));
vi.mock("../components/BetaControls", () => ({
  buttonClass: "",
  primaryClass: "",
  errorText: String,
  Results: () => null,
}));
vi.mock("../components/cleanup/ConfirmDialog", () => ({ default: () => null }));
vi.mock("../store/useAppStore", () => ({
  useAppStore: { getState: () => ({ refreshActionViews: async () => {}, getPendingFiles: async () => {} }) },
}));
vi.mock("react", async (original) => ({
  ...await original<typeof import("react")>(),
  useState(initial: unknown) {
    const slot = harness.cursor++;
    if (!(slot in harness.slots)) harness.slots[slot] = initial;
    return [harness.slots[slot], (value: unknown) => {
      harness.slots[slot] = typeof value === "function" ? value(harness.slots[slot]) : value;
    }];
  },
  useRef(current: unknown) {
    const slot = harness.cursor++;
    if (!(slot in harness.slots)) harness.slots[slot] = { current };
    return harness.slots[slot];
  },
  useEffect(effect: EffectCallback, domains: readonly unknown[]) {
    if (!harness.domains || domains.some((value, index) => !Object.is(value, harness.domains?.[index]))) {
      harness.domains = domains;
      harness.pending = effect;
    }
  },
}));

let scope: string[] | null;
let release: (() => void) | undefined;
const replies: Array<() => Promise<unknown>> = [];
let applyReply: (() => Promise<OperationResult[]>) | undefined;
const settled = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

type ControlProps = {
  children?: ReactNode;
  type?: string;
  checked?: boolean;
  disabled?: boolean;
  count?: number;
  preview?: unknown[];
  results?: OperationResult[];
  onClick?: (event?: { currentTarget: object }) => void;
  onChange?: () => void;
  onConfirm?: () => Promise<void>;
  onClose?: () => void;
};

function render() {
  harness.cursor = 0;
  const root = ReviewQueue({ paths: scope });
  const effect = harness.pending;
  if (effect) {
    harness.pending = undefined;
    release?.();
    release = effect() || undefined;
  }
  const controls: ReactElement<ControlProps>[] = [];
  function visit(node: ReactNode) {
    Children.forEach(node, (child) => {
      if (!isValidElement<ControlProps>(child)) return;
      controls.push(child);
      visit(child.props.children);
    });
  }
  visit(root);
  return controls;
}

function button(label: string) {
  const control = render().find((node) => node.type === "button" && node.props.children === label);
  expect(control, label).toBeDefined();
  return control!.props;
}

function checks() {
  return render().filter((node) => node.type === "input" && node.props.type === "checkbox").map((node) => node.props);
}

function leaveReview() {
  release?.();
  release = undefined;
  harness.slots = [];
  harness.domains = undefined;
  harness.pending = undefined;
}

function approveSelection() {
  button("beta.apply").onClick?.({ currentTarget: {} });
  const action = render().find((node) => node.type === ConfirmDialog)?.props.onConfirm;
  expect(action).toBeDefined();
  return action!;
}

type Row = ReturnType<typeof row>;
function row(sequence: string, file: string, evidence = file, warning = false) {
  return {
    id: sequence + "-" + file,
    selectionKey: evidence,
    source: "C:/Fixture/" + file + ".txt",
    destination: "C:/Fixture/Filed/" + file + ".txt",
    rule: "Fixture rule", action: "move", size: 16, warning, error: null,
  };
}

function serve(id: string, entries: Row[]) {
  replies.push(async () => ({ id, entries }));
}

function chosen() {
  return checks().map((control) => control.checked);
}

async function initial(id: string, entries: Row[]) {
  serve(id, entries);
  render();
  await settled();
}

beforeEach(() => {
  scope = null;
  harness.slots = [];
  harness.domains = undefined;
  harness.pending = undefined;
  replies.length = 0;
  applyReply = undefined;
  harness.invoke.mockReset().mockImplementation((command: string) => {
    if (command === "preview_cmd") {
      const reply = replies.shift();
      if (!reply) throw new Error("Unexpected preview command");
      return reply();
    }
    return command === "apply_preview_cmd" && applyReply ? applyReply() : Promise.resolve(command === "apply_preview_cmd" ? [] : undefined);
  });
});

afterEach(() => {
  leaveReview();
});

it("shows a blocked root with healthy proposals and distinguishes a fully blocked preview from no matches", async () => {
  const blockedRoots = [{
    root: "C:/Fixture/bad-root",
    error: "Unable to read ignore protection file C:/Fixture/bad-root/.mouziignore",
  }];
  replies.push(async () => ({ id: "mixed", entries: [row("healthy", "b")], blockedRoots }));
  render();
  await settled();
  const texts = () => render().map((node) => node.props.children);
  expect(chosen()).toEqual([true]);
  expect(texts()).toContain("beta.unavailableFolders");
  expect(texts()).toContain(blockedRoots[0].root);
  expect(texts()).toContain(blockedRoots[0].error);
  expect(texts()).not.toContain("beta.empty");

  replies.push(async () => ({ id: "blocked", entries: [], blockedRoots }));
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([]);
  expect(texts()).toContain(blockedRoots[0].error);
  expect(texts()).not.toContain("beta.empty");

  serve("empty", []);
  button("beta.refresh").onClick?.();
  await settled();
  expect(texts()).not.toContain(blockedRoots[0].error);
  expect(texts()).toContain("beta.empty");
});

it("keeps first and pristine refresh defaults, excluding warning rows", async () => {
  await initial("first", [row("1", "a"), row("1", "warning", "warning", true)]);
  expect(chosen()).toEqual([true, false]);
  serve("new", [row("2", "b"), row("2", "a"), row("2", "warning", "warning", true)]);
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([true, true, false]);
});

it("keeps one current count and review action together before a long list", async () => {
  const entries = Array.from({ length: 180 }, (_, index) => row("long", `file-${index + 1}`));
  await initial("long", entries);
  expect(chosen()).toHaveLength(180);

  button("beta.clearSelection").onClick?.();
  expect(button("beta.apply").disabled).toBe(true);
  checks()[3].onChange?.();
  checks()[176].onChange?.();
  expect(button("beta.apply").disabled).toBe(false);

  const controls = render();
  const applyActions = controls.filter(node => node.type === "button" && node.props.children === "beta.apply");
  const counts = controls.filter(node => node.props.children === "beta.selected:2");
  expect(applyActions).toHaveLength(1);
  expect(counts).toHaveLength(1);
  const selectionGroup = controls.find(node => {
    const children: ReactNode[] = [];
    Children.forEach(node.props.children, child => { children.push(child); });
    return children.includes(applyActions[0]) && children.includes(counts[0]);
  });
  expect(selectionGroup).toBeDefined();
  const firstCheckbox = controls.findIndex(node => node.type === "input" && node.props.type === "checkbox");
  expect(controls.indexOf(applyActions[0])).toBeLessThan(firstCheckbox);

  button("beta.apply").onClick?.({ currentTarget: {} });
  const confirmation = render().find(node => node.type === ConfirmDialog)?.props;
  expect(confirmation?.count).toBe(2);
  expect(confirmation?.preview).toEqual([entries[3], entries[176]].map(({ source, destination }) => ({ source, destination })));
  confirmation?.onClose?.();
  expect(chosen().filter(Boolean)).toHaveLength(2);
  expect(harness.invoke).not.toHaveBeenCalledWith("apply_preview_cmd", expect.anything());
});

it("carries only checked unchanged ordinary rows, using IDs from the refreshed plan", async () => {
  await initial("first", [row("1", "a"), row("1", "b"), row("1", "warning", "warning", true)]);
  checks()[1].onChange?.();
  checks()[2].onChange?.();
  expect(chosen()).toEqual([true, false, true]);

  serve("new", [row("2", "b"), row("2", "new"), row("2", "warning", "warning", true), row("2", "a")]);
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([false, false, false, true]);

  button("beta.apply").onClick?.({ currentTarget: {} });
  const confirmation = render().find((node) => node.type === ConfirmDialog);
  expect(confirmation).toBeDefined();
  await confirmation!.props.onConfirm?.();
  expect(harness.invoke).toHaveBeenCalledWith("apply_preview_cmd", { id: "new", selected: ["2-a"] });
});

it("keeps Clear selection across refresh and resets it on a new routed scope", async () => {
  await initial("first", [row("1", "a"), row("1", "b")]);
  button("beta.clearSelection").onClick?.();
  serve("refreshed", [row("2", "a"), row("2", "b"), row("2", "new")]);
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([false, false, false]);

  scope = ["C:/Fixture/a.txt"];
  serve("chosen-scope", [row("3", "a")]);
  render();
  await settled();
  expect(chosen()).toEqual([true]);
  expect(harness.invoke).toHaveBeenCalledWith("preview_cmd", { paths: scope });
});

it("retains intent across a failed refresh and excludes changed or missing entries on retry", async () => {
  await initial("first", [row("1", "a"), row("1", "b"), row("1", "c"), row("1", "removed")]);
  checks()[1].onChange?.();
  replies.push(async () => { throw Error("Fixture refresh failure"); });
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([]);

  serve("retry", [row("2", "a"), row("2", "b"), row("2", "c", "changed-content")]);
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([true, false, false]);

  serve("another", [row("3", "a"), row("3", "removed")]);
  button("beta.refresh").onClick?.();
  await settled();
  expect(chosen()).toEqual([true, false]);
});

it("discards a delayed obsolete refresh response after a new routed scope", async () => {
  await initial("first", [row("1", "a"), row("1", "b")]);
  checks()[1].onChange?.();
  let resolveLate!: (value: unknown) => void;
  const late = new Promise((resolve) => { resolveLate = resolve; });
  replies.push(() => late);
  button("beta.refresh").onClick?.();
  await settled();

  scope = ["C:/Fixture/b.txt"];
  serve("new-scope", [row("3", "b")]);
  render();
  await settled();
  expect(chosen()).toEqual([true]);
  resolveLate({ id: "obsolete", entries: [row("2", "a"), row("2", "b")] });
  await settled();
  expect(chosen()).toEqual([true]);
  expect(harness.invoke).toHaveBeenCalledWith("discard_preview_cmd", { id: "obsolete" });
});

it("keeps a submitted plan and collision receipt through navigation until an explicit scoped refresh", async () => {
  const selected = row("held", "approved");
  await initial("held-plan", [selected]);
  const approve = approveSelection();
  let finish!: (results: OperationResult[]) => void;
  applyReply = () => new Promise(resolve => { finish = resolve; });
  const running = approve();
  const outcomes = [{ id: selected.id, source: selected.source, success: true, error: null,
    actualDestination: "C:/Fixture/Filed/approved_0.txt" }];
  try {
    leaveReview();
    expect(harness.invoke).not.toHaveBeenCalledWith("discard_preview_cmd", { id: "held-plan" });
    scope = ["C:/Fixture/next.txt"];
    const pending = render();
    expect(pending.map(node => node.props.children)).toContain("confirm.working");
    expect(button("beta.refresh").disabled).toBe(true);
    expect(pending.some(node => node.type === "button" && node.props.children === "beta.apply")).toBe(false);
    void approve();
    expect(harness.invoke.mock.calls.filter(([command]) => command === "apply_preview_cmd"))
      .toEqual([["apply_preview_cmd", { id: "held-plan", selected: ["held-approved"] }]]);
  } finally {
    finish(outcomes);
    await running;
  }
  expect(render().find(node => node.type === Results)?.props.results).toEqual(outcomes);
  leaveReview();
  expect(render().find(node => node.type === Results)?.props.results).toEqual(outcomes);
  expect(harness.invoke.mock.calls.filter(([command]) => command === "preview_cmd")).toEqual([["preview_cmd", { paths: null }]]);

  serve("next-plan", [row("next", "next")]);
  button("beta.refresh").onClick?.();
  await settled();
  expect(render().find(node => node.type === Results)?.props.results).toEqual([]);
  expect(harness.invoke).toHaveBeenCalledWith("preview_cmd", { paths: ["C:/Fixture/next.txt"] });
  expect(chosen()).toEqual([true]);
});

it("keeps the exact rejected apply outcome when Review is opened again", async () => {
  await initial("failing-plan", [row("failing", "a")]);
  let fail!: (error: string) => void;
  applyReply = () => new Promise((_, reject) => { fail = reject; });
  const running = approveSelection()();
  leaveReview();
  fail("The reviewed destination is unavailable");
  await running;
  render();
  await settled();
  expect(render().map(node => node.props.children)).toContain("The reviewed destination is unavailable");
  expect(harness.invoke).not.toHaveBeenCalledWith("discard_preview_cmd", { id: "failing-plan" });
});

it("retires an unconfirmed plan when leaving Review", async () => {
  await initial("unconfirmed-plan", [row("unconfirmed", "a")]);
  approveSelection();
  leaveReview();
  expect(harness.invoke).toHaveBeenCalledWith("discard_preview_cmd", { id: "unconfirmed-plan" });
  expect(harness.invoke.mock.calls.some(([command]) => command === "apply_preview_cmd")).toBe(false);
});
