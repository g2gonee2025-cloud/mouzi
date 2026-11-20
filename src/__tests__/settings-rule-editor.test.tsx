import { isValidElement, type ReactElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Settings from "../components/Settings";
import { useAppStore, type Rule } from "../store/useAppStore";

const harness = vi.hoisted(() => ({ invoke: vi.fn(), slots: [] as unknown[], cursor: 0 }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: harness.invoke }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../i18n", () => ({
  default: { t: (key: string, options?: { defaultValue?: string }) => options?.defaultValue ?? key, language: "en" },
}));
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  function claim(initialize: () => unknown) {
    const index = harness.cursor++;
    if (!(index in harness.slots)) harness.slots[index] = initialize();
    return index;
  }
  return {
    ...actual,
    useEffect: () => {},
    useState: (initial: unknown) => {
      const index = claim(() => typeof initial === "function" ? initial() : initial);
      return [
        harness.slots[index],
        (next: unknown) => { harness.slots[index] = typeof next === "function" ? next(harness.slots[index]) : next; },
      ];
    },
    useRef: (initial: unknown) => harness.slots[claim(() => ({ current: initial }))],
  };
});
vi.mock("../store/useAppStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useAppStore")>();
  const store = actual.useAppStore;
  return { ...actual, useAppStore: Object.assign(store.getState, store) };
});

interface Props {
  children?: ReactNode;
  value?: string;
  checked?: boolean;
  label?: string;
  role?: string;
  title?: string;
  "aria-label"?: string;
  onClick?: (event: { currentTarget: HTMLButtonElement }) => unknown;
  onChange?: (event: { target: { value?: string; checked?: boolean } }) => void;
  onConfirm?: () => Promise<void>;
  onClose?: () => void;
}
type Element = ReactElement<Props>;
type Match = (element: Element) => boolean;

function text(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(text).join("");
  if (typeof node === "string") return node;
  return isValidElement<Props>(node) ? text(node.props.children) : "";
}

function find(node: ReactNode, matches: Match): Element | undefined {
  if (Array.isArray(node)) return node.map((child) => find(child, matches)).find(Boolean);
  if (!isValidElement<Props>(node)) return;
  return matches(node) ? node : find(node.props.children, matches);
}

function editor() {
  harness.cursor = 0;
  return Settings();
}

function requireElement(matches: Match, tree: ReactNode = editor()) {
  const element = find(tree, matches);
  if (!element) throw new Error("Fixture control is absent");
  return element;
}

const button = (label: string): Match => (node) => node.type === "button" && text(node.props.children) === label;
const nameField: Match = (node) => node.type === "input" && node.props["aria-label"] === "settings.rules.name";
const field = (label: string) => requireElement((node) => node.type === "input" && node.props["aria-label"] === label);
const checkbox = (label: string) => requireElement(
  (node) => node.type === "input" && !!node.props.onChange,
  requireElement((node) => node.type === "label" && text(node.props.children) === label),
);
const click = (node: Element) => node.props.onClick!({ currentTarget: {} as HTMLButtonElement });
const edit = (name: string) => click(requireElement(({ props }) => props["aria-label"] === `settings.rules.edit: ${name}`));
const toggle = (name: string) => click(requireElement(({ props }) => props.role === "switch" && props["aria-label"]?.startsWith(`${name}: `) === true));
const change = (node: Element, target: { value?: string; checked?: boolean }) => node.props.onChange!({ target });
const submit = () => click(requireElement((node) => button("settings.rules.save")(node) || button("settings.rules.edit")(node)));
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

async function remove(name: string) {
  click(requireElement(({ props }) => props["aria-label"] === `confirm.op.deleteRule: ${name}`));
  const confirm = requireElement(({ props }) => props.title === "confirm.op.deleteRule" && !!props.onConfirm);
  await confirm.props.onConfirm!();
  confirm.props.onClose!();
}

const invoice: Rule = { id: 1, name: "Invoices", priority: 4, enabled: true, extensions: ["fixture"], pattern: null, destination: "", action: "ignore", folder_id: 0 };
const receipts: Rule = { ...invoice, id: 2, name: "Receipts", priority: 6 };
const db = new Map<number, Rule>();
let nextId = 10;
let ruleReadFails = false;

beforeEach(() => {
  harness.slots.length = 0;
  harness.invoke.mockReset();
  db.clear();
  db.set(1, { ...invoice });
  db.set(2, { ...receipts });
  nextId = 10;
  ruleReadFails = false;
  harness.invoke.mockImplementation(async (command: string, args?: { rule?: Rule; id?: number; replace?: boolean }) => {
    if (command === "get_rules_cmd") {
      if (ruleReadFails) throw "Fixture rule list unavailable";
      return [...db.values()];
    }
    if (command === "add_rule_cmd" && args?.rule) {
      const id = nextId++;
      db.set(id, { ...args.rule, id });
      return id;
    }
    if (command === "update_rule_cmd" && args?.rule?.id !== undefined) {
      if (!db.has(args.rule.id)) throw "This rule no longer exists";
      db.set(args.rule.id, { ...args.rule });
      return;
    }
    if (command === "delete_rule_cmd" && args?.id !== undefined) {
      db.delete(args.id);
      return;
    }
    if (command === "import_rules_cmd" && args?.replace) {
      db.clear();
      const id = nextId++;
      db.set(id, { ...invoice, id, name: "Replacement" });
      return 1;
    }
    throw new Error(`Unexpected fixture command ${command}`);
  });
  useAppStore.setState(useAppStore.getInitialState(), true);
  useAppStore.setState({ rules: [...db.values()] });
  click(requireElement(({ props }) => props.label === "settings.rules.title"));
});

afterEach(() => {
  vi.useRealTimers();
});

it("keeps both a same-row toggle and other changes from the open draft", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Invoices revised" });
  change(field("settings.rules.priority"), { value: "17" });
  toggle("Invoices");
  await settle();
  expect(db.get(1)?.enabled).toBe(false);
  const draftEnabledAfterToggle = checkbox("settings.rules.enabled").props.checked;
  await submit();

  expect(db.get(1)).toEqual({ ...invoice, name: "Invoices revised", priority: 17, enabled: false });
  expect(db.get(2)).toEqual(receipts);
  expect(draftEnabledAfterToggle).toBe(false);
  edit("Invoices revised");
  expect(checkbox("settings.rules.enabled").props.checked).toBe(false);
});

it("honors a later editor enable change and saves a different row independently", async () => {
  edit("Invoices");
  toggle("Invoices");
  await settle();
  change(checkbox("settings.rules.enabled"), { checked: true });
  toggle("Receipts");
  await settle();
  expect(checkbox("settings.rules.enabled").props.checked).toBe(true);
  await submit();

  expect(db.get(1)).toEqual(invoice);
  expect(db.get(2)).toEqual({ ...receipts, enabled: false });
});

it("cancel discards the name draft while keeping the committed switch on reopen", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Abandoned name" });
  toggle("Invoices");
  await settle();
  click(requireElement(button("common.cancel")));

  expect(db.get(1)).toEqual({ ...invoice, enabled: false });
  edit("Invoices");
  expect(field("settings.rules.name").props.value).toBe("Invoices");
  expect(checkbox("settings.rules.enabled").props.checked).toBe(false);
});

it("dismisses the confirmed deleted rule draft and preserves a different draft", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Unsaved invoice draft" });
  await remove("Receipts");
  expect(field("settings.rules.name").props.value).toBe("Unsaved invoice draft");
  await remove("Invoices");

  expect(db.size).toBe(0);
  expect(find(editor(), nameField)).toBeUndefined();
});

it("dismisses a replaced saved draft and preserves an unrelated new draft", async () => {
  vi.useFakeTimers();
  edit("Invoices");
  change(checkbox("settings.rules.replaceOnImport"), { checked: true });
  await click(requireElement(button("settings.rules.import")));
  expect(db.has(1)).toBe(false);
  expect(find(editor(), nameField)).toBeUndefined();

  click(requireElement(button("settings.rules.add")));
  change(field("settings.rules.name"), { value: "Unsaved new draft" });
  await click(requireElement(button("settings.rules.import")));
  expect(field("settings.rules.name").props.value).toBe("Unsaved new draft");
});

it("labels the submit action Save and reserves Edit for opening rows", () => {
  edit("Invoices");
  expect(find(editor(), button("settings.rules.save"))).toBeDefined();
  expect(find(editor(), button("settings.rules.edit"))).toBeUndefined();
});

it("shows a missing-target rejection without closing the unsaved draft", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Unsaved invoice draft" });
  db.delete(1);
  await submit();

  expect(field("settings.rules.name").props.value).toBe("Unsaved invoice draft");
  expect(text(requireElement(({ props }) => props.role === "alert"))).toBe("This rule no longer exists");
  expect(db.has(1)).toBe(false);
});

it("retains an acknowledged switch when the subsequent rule list read fails", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Invoices revised" });
  ruleReadFails = true;
  toggle("Invoices");
  await settle();
  expect(db.get(1)?.enabled).toBe(false);
  await submit();

  expect(db.get(1)).toEqual({ ...invoice, name: "Invoices revised", enabled: false });
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
});

it("finishes an acknowledged new rule despite a failed reload and retries only the list", async () => {
  click(requireElement(button("settings.rules.add")));
  change(field("settings.rules.name"), { value: "New fixture rule" });
  change(field("settings.rules.extensions"), { value: "mzt" });
  change(field("settings.rules.destination"), { value: "Sorted" });
  ruleReadFails = true;
  await submit();

  expect([...db.values()].filter((rule) => rule.name === "New fixture rule")).toHaveLength(1);
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
  ruleReadFails = false;
  await click(requireElement(button("common.retry")));
  expect(text(editor())).not.toContain("settings.rules.refreshAfterWriteError");
  expect(harness.invoke.mock.calls.filter(([command]) => command === "add_rule_cmd")).toHaveLength(1);
});

it("invalidates a replaced persisted draft even when its post-commit list read fails", async () => {
  vi.useFakeTimers();
  edit("Invoices");
  change(checkbox("settings.rules.replaceOnImport"), { checked: true });
  ruleReadFails = true;
  await click(requireElement(button("settings.rules.import")));

  expect(db.has(1)).toBe(false);
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
});

it("dismisses an acknowledged deleted draft even if the new list was unavailable", async () => {
  edit("Invoices");
  ruleReadFails = true;
  await expect(remove("Invoices")).resolves.toBeUndefined();

  expect(db.has(1)).toBe(false);
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
});

it("does not allow an older list read to restore a previous rule state", async () => {
  let finishOldRead!: (rows: Rule[]) => void;
  const oldReply = new Promise<Rule[]>((resolve) => { finishOldRead = resolve; });
  harness.invoke.mockReturnValueOnce(oldReply);
  const stale = useAppStore.getState().loadRules();
  toggle("Invoices");
  await settle();

  finishOldRead([invoice, receipts]);
  await stale;
  expect(useAppStore.getState().rules.find(({ id }) => id === 1)?.enabled).toBe(false);
});
