import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Children, isValidElement, type EffectCallback, type ReactNode, type ReactElement } from "react";
import ReviewQueue from "../components/ReviewQueue";
import ConfirmDialog from "../components/cleanup/ConfirmDialog";

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
const settled = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

type ControlProps = {
  children?: ReactNode;
  type?: string;
  checked?: boolean;
  disabled?: boolean;
  count?: number;
  preview?: unknown[];
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
  harness.invoke.mockReset().mockImplementation((command: string) => {
    if (command === "preview_cmd") {
      const reply = replies.shift();
      if (!reply) throw new Error("Unexpected preview command");
      return reply();
    }
    return Promise.resolve(command === "apply_preview_cmd" ? [] : undefined);
  });
});

afterEach(() => {
  release?.();
  release = undefined;
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
