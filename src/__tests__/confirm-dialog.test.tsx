import { renderToStaticMarkup } from "react-dom/server";
import { beforeAll, expect, it } from "vitest";
import ConfirmDialog from "../components/cleanup/ConfirmDialog";
import { initI18n } from "../i18n";

beforeAll(() => initI18n("en"));

function renderConfirm(count: number) {
  return renderToStaticMarkup(
    <ConfirmDialog title="Remove empty folders" count={count} notice="permanent"
      onConfirm={() => {}} onClose={() => {}} />,
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

it("keeps the destructive control disabled until a typed confirmation matches", () => {
  const markup = renderConfirm(26);
  const input = markup.match(/<input\b[^>]*>/)?.[0] ?? "";
  const id = input.match(/\sid="([^"]+)"/)?.[1];
  expect(id).toBeTruthy();
  expect(markup).toContain(`<label for="${id}"`);
  expect(markup).toMatch(/<button\b[^>]*\sdisabled=""[^>]*>Remove empty folders<\/button>/);
});

it("shows each source and destination in one capped row and retains the consent tier", () => {
  const pairs = Array.from({ length: 26 }, (_, index) => ({
    source: `C:/Fixture/inputs/source-${index}.txt`,
    destination: `C:/Fixture/Sorted/renamed-${index}.txt`,
  }));
  const markup = renderToStaticMarkup(
    <ConfirmDialog title="Apply 26 operations" count={pairs.length} notice="move"
      preview={pairs} onConfirm={() => {}} onClose={() => {}} />,
  );
  const items = markup.match(/<li\b[^>]*>[\s\S]*?<\/li>/g) ?? [];
  expect(items).toHaveLength(8);
  for (const [index, item] of items.entries()) {
    expect(item).toContain(pairs[index].source);
    expect(item).toContain(pairs[index].destination);
  }
  expect(markup).not.toContain(pairs[8].destination);
  expect(markup).toContain("+18 more");
  expect(markup).toContain("Type DELETE to confirm.");
  expect(markup).toMatch(/<button\b[^>]*\sdisabled=""[^>]*>Apply 26 operations<\/button>/);
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
