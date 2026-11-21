import { Children, isValidElement, type ReactNode, type SetStateAction } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeAll, expect, it, vi } from "vitest";
import ConfirmDialog, { type ConfirmDialogProps } from "../components/cleanup/ConfirmDialog";
import { initI18n } from "../i18n";

const stateFrame = vi.hoisted(() => ({
  active: null as { slots: unknown[]; cursor: number } | null,
}));

vi.mock("react", async (importOriginal) => {
  const react = await importOriginal<typeof import("react")>();
  return {
    ...react,
    useState: <T,>(initial: T | (() => T)) => {
      const frame = stateFrame.active;
      if (!frame) return react.useState(initial);
      const slot = frame.cursor++;
      if (!(slot in frame.slots)) frame.slots[slot] = typeof initial === "function" ? (initial as () => T)() : initial;
      const update = (next: SetStateAction<T>) => {
        frame.slots[slot] = typeof next === "function" ? (next as (previous: T) => T)(frame.slots[slot] as T) : next;
      };
      return [frame.slots[slot], update] as const;
    },
  };
});

beforeAll(() => initI18n("en"));

function renderConfirm(count: number, impact: Pick<ConfirmDialogProps, "bytes" | "countIsExact"> = {}) {
  return renderToStaticMarkup(
    <ConfirmDialog title="Remove empty folders" count={count} notice="permanent"
      onConfirm={() => {}} onClose={() => {}} {...impact} />,
  );
}

it("renders a closed native dialog with its title and consequences linked", () => {
  const markup = renderConfirm(1);
  const dialog = markup.match(/<dialog\b[^>]*>/)?.[0] ?? "";
  const label = dialog.match(/aria-labelledby="([^"]+)"/)?.[1];
  const description = dialog.match(/aria-describedby="([^"]+)"/)?.[1];
  expect(label).toBeTruthy();
  expect(description).toBeTruthy();
  expect(dialog).not.toMatch(/\sopen(?:=|\s|>)/);
  expect(markup).toContain(`<h2 id="${label}"`);
  expect(markup).toContain(`<div id="${description}"`);
});

it("reports all three items in a simple confirmation even if size is unavailable", () => {
  const markup = renderConfirm(3);
  expect(markup).toContain("3 items");
  expect(markup).toContain("Total size unavailable.");
});

it.each([3, 10, 26])("states the minimum when only %i items are known", (count) => {
  const markup = renderConfirm(count, { countIsExact: false });
  expect(markup).toContain(`At least ${count} items`);
});

it.each([[0, "0 B"], [2048, "2.00 KiB"]])("shows the known size for %i bytes in a simple confirmation", (bytes, size) => {
  const markup = renderConfirm(3, { bytes });
  expect(markup).toContain(`Total size: ${size}`);
  expect(markup).not.toContain("Total size unavailable.");
});

it("disables an exact zero selection and leaves Cancel enabled", () => {
  const markup = renderConfirm(0);
  expect(markup).toContain(">0 items<");
  expect(markup).toMatch(/<button\b[^>]*\sdisabled=""[^>]*>Remove empty folders<\/button>/);
  expect(markup).toMatch(/<button\b(?![^>]*\sdisabled=)[^>]*>Cancel<\/button>/);
});

it("allows an operation with no known item minimum to proceed and shows its known size", () => {
  const markup = renderConfirm(0, { countIsExact: false, bytes: 0 });
  expect(markup).not.toContain("0 items");
  expect(markup).toContain("Total size: 0 B");
  expect(markup).toMatch(/<button\b(?![^>]*\sdisabled=)[^>]*>Remove empty folders<\/button>/);
});

it("renders the count and unavailable size in the selected language", async () => {
  try {
    await initI18n("de");
    const markup = renderConfirm(3);
    expect(markup).toContain("3 Elemente");
    expect(markup).toContain("Gesamtgröße nicht verfügbar.");
    expect(markup).not.toMatch(/confirm\.(?:impactItems|bytesUnknown)|Total size unavailable/);
  } finally {
    await initI18n("en");
  }
});

it("keeps the destructive control disabled until a typed confirmation matches", () => {
  const markup = renderConfirm(26);
  const input = markup.match(/<input\b[^>]*>/)?.[0] ?? "";
  const id = input.match(/\sid="([^"]+)"/)?.[1];
  expect(id).toBeTruthy();
  expect(markup).toContain(`<label for="${id}"`);
  expect(markup).toMatch(/<button\b[^>]*\sdisabled=""[^>]*>Remove empty folders<\/button>/);
});

interface DialogControl {
  children?: ReactNode;
  onClick?: () => void;
  onChange?: (event: { target: { value: string } }) => void;
  value?: string;
  disabled?: boolean;
  "aria-disabled"?: boolean;
}

it("lets a person review every paired move within bounded pages and grants consent only for the same plan", () => {
  const makeItems = (folder: string, count: number) => Array.from({ length: count }, (_, index) => ({
    source: "C:/Fixture/" + folder + "/inputs/source-" + index + ".txt",
    destination: "C:/Fixture/" + folder + "/Sorted/renamed-" + index + ".txt",
  }));
  const moves = makeItems("first", 33);
  const replacement = makeItems("replacement", 26);
  const apply = vi.fn(), applyReplacement = vi.fn(), cancel = vi.fn();
  let props: ConfirmDialogProps = { title: "Apply operations", count: moves.length, preview: moves, notice: "move",
    bytes: 2048, onConfirm: apply, onClose: cancel };
  const frame = { slots: [] as unknown[], cursor: 0 };
  const buttons = new Map<string, DialogControl>();
  let typed: DialogControl | undefined;

  function capture(node: ReactNode) {
    Children.forEach(node, (child) => {
      if (!isValidElement<DialogControl>(child)) return;
      if (child.type === "button" && typeof child.props.children === "string") buttons.set(child.props.children, child.props);
      if (child.type === "input") typed = child.props;
      capture(child.props.children);
    });
  }

  function render() {
    function CaptureDialog() {
      const tree = ConfirmDialog(props);
      capture(tree);
      return tree;
    }

    buttons.clear();
    typed = undefined;
    frame.cursor = 0;
    stateFrame.active = frame;
    try { return renderToStaticMarkup(<CaptureDialog />); }
    finally { stateFrame.active = null; }
  }

  function checkPage(markup: string, first: number, last: number) {
    const rows = markup.match(/<li\b[^>]*>[\s\S]*?<\/li>/g) ?? [];
    const expected = moves.slice(first, last);
    expect(rows).toHaveLength(expected.length);
    for (const [index, item] of rows.entries()) {
      expect(item).toContain(expected[index].source);
      expect(item).toContain(expected[index].destination);
    }
  }

  const first = render();
  checkPage(first, 0, 8);
  expect(first).toContain("33 items");
  expect(first).toContain("Total size: 2.00 KiB");
  expect(first).toContain("Type DELETE to confirm.");
  expect(buttons.get("Apply operations")?.disabled).toBe(true);
  expect([...buttons.keys()]).toContain("Next page");
  expect(buttons.get("Previous page")?.["aria-disabled"]).toBe(true);

  typed?.onChange?.({ target: { value: "DELETE" } });
  render();
  expect(buttons.get("Apply operations")?.disabled).toBe(false);
  for (let page = 1; page < 5; page += 1) {
    buttons.get("Next page")?.onClick?.();
    const next = render();
    expect(next).toContain("Page " + (page + 1) + " of 5");
    checkPage(next, page * 8, Math.min((page + 1) * 8, moves.length));
    expect(typed?.value).toBe("DELETE");
    expect(buttons.get("Apply operations")?.disabled).toBe(false);
  }
  expect(buttons.get("Next page")?.["aria-disabled"]).toBe(true);
  buttons.get("Previous page")?.onClick?.();
  checkPage(render(), 24, 32);

  props = { ...props, count: replacement.length, preview: replacement, onConfirm: applyReplacement };
  const replaced = render();
  expect(replaced).toContain("Page 1 of 4");
  expect(replaced).toContain(replacement[0].destination);
  expect(replaced).not.toContain(moves[0].destination);
  expect(typed?.value).toBe("");
  expect(buttons.get("Apply operations")?.disabled).toBe(true);
  buttons.get("Cancel")?.onClick?.();
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(apply).not.toHaveBeenCalled();
  expect(applyReplacement).not.toHaveBeenCalled();
});

it("keeps a path-only consumer unchanged", () => {
  const item = "C:/Fixture/Remove this empty folder";
  const markup = renderToStaticMarkup(
    <ConfirmDialog title="Remove folder" count={1} notice="permanent"
      preview={[item]} onConfirm={() => {}} onClose={() => {}} />,
  );
  expect(markup.match(/<li\b[^>]*>[\s\S]*?<\/li>/g)).toHaveLength(1);
  expect(markup).toContain(`title="${item}"`);
});
