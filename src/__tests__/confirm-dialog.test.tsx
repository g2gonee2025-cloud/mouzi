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
