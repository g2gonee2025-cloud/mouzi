import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, writeFile, readFile, access, realpath } from 'node:fs/promises';
import { resolve, join, basename } from 'node:path';
import { createHash } from 'node:crypto';
const { chromium } = await import(process.env.MOUZI_PLAYWRIGHT_MODULE || 'playwright');
const pl = JSON.parse(await readFile(new URL('../src/i18n/locales/pl.json', import.meta.url), 'utf8'));
const version = process.env.MOUZI_TEST_VERSION || '0.2.0-beta.2';
const stable = !version.includes('beta');
if (stable && (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true')) throw Error('Stable UI tests require an isolated GitHub Actions runner. Do not use a personal stable profile.');
const exe = resolve(process.env.MOUZI_TEST_EXE || 'artifacts/0.2.0-beta.2/Mouzi-0.2.0-beta.2-windows-x64.exe');
const run = resolve(`artifacts/${version}/private-tests/${Date.now()}`);
const fixture = join(run, 'pliki testowe — Łódź');
await mkdir(fixture, { recursive: true });
const report = { exe, sha256: createHash('sha256').update(await readFile(exe)).digest('hex'), started: new Date().toISOString(), cases: [], errors: [], cleanup: [] };
let child, browser, page, originalSettings, folderId, originalLogs, originalRules, originalFolders;
const ciPolicyKey = 'HKLM\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments';
let ciPolicyInstalled = false;
const ownedRules = new Set(), ownedFolders = new Set();
const exists = path => access(path).then(() => true, () => false);
const invoke = (cmd, args = {}) => page.evaluate(({ cmd, args }) => window.__TAURI_INTERNALS__.invoke(cmd, args), { cmd, args });
const button = name => page.getByRole('button', { name, exact: true });
const field = name => page.getByText(name, { exact: true }).locator('..').locator('input').first();
async function until(check, message, timeout = 15000) { const end = Date.now() + timeout; while (Date.now() < end) { if (await check()) return; await new Promise(r => setTimeout(r, 150)); } throw Error(message); }
async function start() {
  child = spawn(exe, stable ? ['--add-folder', fixture] : [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=19221 --remote-debugging-address=127.0.0.1' } });
  child.stdout.on('data', chunk => console.log(String(chunk)));
  child.stderr.on('data', chunk => console.error(String(chunk)));
  try {
    await until(async () => { if (child.exitCode !== null) throw Error(`Application exited during startup: ${child.exitCode}`); try { return (await fetch('http://127.0.0.1:19221/json/version', { signal: AbortSignal.timeout(2000) })).ok; } catch { return false; } }, 'Application debug endpoint did not start', stable ? 120000 : 15000);
  } catch (error) {
    if (stable) {
      try { console.log(execFileSync('powershell.exe', ['-NoProfile', '-File', 'tests/capture-ci-desktop.ps1'], { encoding: 'utf8', env: { ...process.env, MOUZI_CAPTURE_DIR: run, MOUZI_CAPTURE_PID: String(child.pid) } })); } catch (captureError) { console.error(String(captureError)); }
    }
    throw error;
  }
  browser = await chromium.connectOverCDP('http://127.0.0.1:19221');
  await until(async () => { page = browser.contexts().flatMap(c => c.pages()).find(p => p.url().includes('settings')); return !!page; }, 'Settings window missing');
  page.on('pageerror', error => report.errors.push(error.message));
  await page.waitForFunction(() => !!window.__TAURI_INTERNALS__);
}
async function stop() { await browser?.close(); child?.kill(); await until(() => Promise.resolve(child.exitCode !== null || child.signalCode !== null), 'Test process did not exit'); }
async function caseOf(name, fn) {
  try { await fn(); report.cases.push({ name, status: 'PASS' }); console.log(`PASS ${name}`); }
  catch (error) { report.cases.push({ name, status: 'FAIL', error: String(error) }); await page?.screenshot({ path: join(run, 'failure.png') }).catch(() => {}); throw error; }
}
async function review(paths) {
  await invoke('show_review_cmd', { paths });
  await page.getByRole('heading', { name: 'Review queue', exact: true }).waitFor();
  await until(() => button('Refresh preview').isEnabled(), 'Preview remained busy');
  // A new request can arrive while the existing queue is mounted.
  await button('Refresh preview').click();
  await until(() => button('Refresh preview').isEnabled(), 'Preview did not refresh');
}
async function applySelected(count) {
  await button('Review selected operations').click();
  await button(`Apply ${count} operations`).click();
  await page.getByRole('heading', { name: 'Operation results', exact: true }).waitFor();
}
async function editRule(ruleName) {
  await button('Rules').click();
  const row = page.getByText(ruleName, { exact: true }).locator('..').locator('..').locator('..');
  await row.getByRole('button', { name: 'Edit', exact: true }).click();
}
async function saveRule(name) {
  await button('Save').click();
  await until(async () => { const rules = await invoke('get_rules_cmd'); const rule = rules.find(r => r.name === name); if (rule) { ownedRules.add(rule.id); return true; } return false; }, 'Rule was not saved');
}
async function measureContrast(locator) {
  return locator.evaluate(node => {
    const text = getComputedStyle(node).color;
    let parent = node, background = 'transparent';
    while (parent && (background === 'transparent' || background === 'rgba(0, 0, 0, 0)')) {
      background = getComputedStyle(parent).backgroundColor; parent = parent.parentElement;
    }
    if (background === 'transparent' || background === 'rgba(0, 0, 0, 0)') background = getComputedStyle(document.documentElement).getPropertyValue('--color-surface-val').trim();
    const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
    const ctx = canvas.getContext('2d');
    const rgb = color => { ctx.clearRect(0, 0, 1, 1); ctx.fillStyle = color; ctx.fillRect(0, 0, 1, 1); return [...ctx.getImageData(0, 0, 1, 1).data].slice(0, 3); };
    const luminance = channels => channels.map(c => c / 255).map(c => c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4).reduce((sum, c, i) => sum + c * [.2126, .7152, .0722][i], 0);
    const foregroundRgb = rgb(text), backgroundRgb = rgb(background);
    const a = luminance(foregroundRgb), b = luminance(backgroundRgb);
    return { text, background, foregroundRgb, backgroundRgb, ratio: (Math.max(a, b) + .05) / (Math.min(a, b) + .05), systemDark: matchMedia('(prefers-color-scheme: dark)').matches, appDark: document.documentElement.classList.contains('dark') };
  });
}
try {
  if (stable) {
    // Elevated CI runners require machine policy; WebView2 150+ ignores environment overrides.
    execFileSync('reg.exe', ['add', ciPolicyKey, '/v', basename(exe), '/t', 'REG_SZ', '/d', '--remote-debugging-port=19221 --remote-debugging-address=127.0.0.1', '/f']);
    ciPolicyInstalled = true;
  }
  await start();
  originalSettings = await invoke('get_settings_cmd'); originalLogs = await invoke('get_logs_cmd', { limit: -1 });
  originalRules = await invoke('get_rules_cmd'); originalFolders = await invoke('get_folders_cmd');
  await invoke('update_settings_cmd', { settings: { ...originalSettings, language: 'en' } });
  await page.reload(); await button('About').waitFor();
  await caseOf('Launch the exact portable EXE; About/version and updater availability', async () => {
    assert.equal((await invoke('get_version_cmd'))[0], version);
    await button('About').click(); await page.getByText(version, { exact: true }).waitFor();
    const buttons = page.getByRole('button').filter({ hasText: /Check.*[Uu]pdate/ });
    assert.equal(await buttons.first().isDisabled(), !stable);
    await page.screenshot({ path: join(run, '01-about.png'), animations: 'disabled' });
  });
  await caseOf('Add a folder with spaces and Polish characters through the UI', async () => {
    await button('Watched Folders').click();
    await page.getByPlaceholder('C:/Users/.../Downloads').fill(fixture);
    await page.getByLabel('Mode for the new folder').selectOption('suggest');
    await button('Add Folder').click();
    await until(async () => { const folder = (await invoke('get_folders_cmd')).find(f => f.path === fixture); if (folder) { folderId = folder.id; ownedFolders.add(folder.id); return folder.mode === 'suggest'; } return false; }, 'Folder was not added in Suggest mode');
  });
  const ruleName = `Private UI ${Date.now()}`;
  await caseOf('UI rule editor: invalid regex, invalid size range, normalized extension list', async () => {
    await button('Rules').click(); await button('Add Rule').click();
    await field('Name').fill(ruleName); await page.getByPlaceholder('Extensions', { exact: true }).fill('mzt, ,');
    await field('Destination').fill('Wynik — dokumenty'); await field('Priority').fill('-100');
    await field('Pattern').fill('['); await button('Save').click();
    await page.getByRole('alert').filter({ hasText: 'regular expression' }).waitFor();
    await field('Pattern').fill('');
    await page.getByLabel('Minimum size (bytes)', { exact: true }).fill('20');
    await page.getByLabel('Maximum size (bytes)', { exact: true }).fill('10'); await button('Save').click();
    await page.getByRole('alert').filter({ hasText: 'Minimum size' }).waitFor();
    await page.getByLabel('Minimum size (bytes)', { exact: true }).fill('1');
    await page.getByLabel('Maximum size (bytes)', { exact: true }).fill('100');
    await saveRule(ruleName);
    const rule = (await invoke('get_rules_cmd')).find(r => r.name === ruleName); assert.deepEqual(rule.extensions, ['mzt']);
  });
  const a = join(fixture, 'faktura A.mzt'), b = join(fixture, 'faktura B.mzt'), warning = join(fixture, 'invoice.pdf.exe');
  await writeFile(a, 'invoice alpha'); await writeFile(b, 'invoice beta'); await writeFile(warning, 'This is an inert text fixture, never executed.');
  await caseOf('Suggest queue, unchecked suspicious filename, selection and cancellation', async () => {
    await until(async () => (await invoke('get_pending_files_cmd')).some(([, name]) => name === 'faktura A.mzt'), 'Suggest queue did not see file');
    assert.equal(await exists(a), true); await review([a, b, warning]);
    const labels = page.locator('section label');
    assert.equal(await labels.filter({ hasText: 'invoice.pdf.exe' }).getByRole('checkbox').isChecked(), false);
    await labels.filter({ hasText: 'faktura B.mzt' }).getByRole('checkbox').uncheck();
    await button('Review selected operations').click(); assert.equal(await exists(a), true);
    await button('Cancel').click(); assert.equal(await exists(a), true);
    await page.screenshot({ path: join(run, '02-preview.png'), animations: 'disabled' });
    await applySelected(1); assert.equal(await exists(a), false); assert.equal(await exists(b), true);
    assert.equal(await readFile(join(fixture, 'Wynik — dokumenty', 'faktura A.mzt'), 'utf8'), 'invoice alpha');
  });
  await caseOf('History filter and safe undo with a conflicting original filename', async () => {
    await writeFile(a, 'unrelated conflict'); await button('History').click();
    await page.getByLabel('Filename', { exact: true }).fill('faktura A.mzt'); await button('Filter history').click();
    await button('Select eligible files').click(); await button('Review undo').click(); await button('Restore 1 files').click();
    await page.getByText('Restored under a different name', { exact: true }).waitFor();
    const restored = page.getByText('Now at ', { exact: false });
    const restoredPath = (await restored.textContent()).slice('Now at '.length);
    assert.equal(await realpath(restoredPath), await realpath(join(fixture, 'faktura A_0.mzt')));
    assert.equal(await readFile(a, 'utf8'), 'unrelated conflict');
    assert.equal(await readFile(restoredPath, 'utf8'), 'invoice alpha');
    assert.equal(await exists(join(fixture, 'Wynik — dokumenty', 'faktura A.mzt')), false);
    await page.screenshot({ path: join(run, '03-undo-conflict.png'), animations: 'disabled' });
  });
  await caseOf('A changed file is rejected after preview, with a readable UI error', async () => {
    await review([b]); await writeFile(b, 'changed since preview'); await applySelected(1);
    await page.getByText('The file, folder or rule changed. Refresh the preview.', { exact: true }).waitFor();
    assert.equal(await readFile(b, 'utf8'), 'changed since preview');
  });
  await caseOf('Rule row enable/disable control and true rename through the UI', async () => {
    await button('Rules').click();
    const toggle = page.getByRole('switch', { name: `${ruleName}: Enabled`, exact: true }); await toggle.click();
    await until(async () => !(await invoke('get_rules_cmd')).find(r => r.name === ruleName).enabled, 'Rule toggle did not save');
    await page.getByRole('switch', { name: `${ruleName}: OFF`, exact: true }).click();
    await editRule(ruleName);
    await page.getByText('Action', { exact: true }).locator('..').locator('select').selectOption('rename');
    await page.getByLabel('Filename template', { exact: false }).fill('../{stem}'); await button('Save').click();
    await page.getByRole('alert').filter({ hasText: 'template' }).waitFor();
    await page.getByLabel('Filename template', { exact: false }).fill('gotowe-{stem}.{extension}'); await button('Save').click();
    await until(async () => (await invoke('get_rules_cmd')).find(r => r.name === ruleName).action === 'rename', 'Rename action did not save');
    await review([b]); await applySelected(1);
    const renamed = join(fixture, 'gotowe-faktura B.mzt'); assert.equal(await exists(b), false); assert.equal(await readFile(renamed, 'utf8'), 'changed since preview');
    assert.equal((await invoke('get_pending_files_cmd')).some(([, name]) => name === 'gotowe-faktura B.mzt'), false);
  });
  if (stable && originalLogs.length === 0) {
    await caseOf('History refreshes after Restore All and Clear History without losing the applied filter', async () => {
      const logs = await invoke('get_logs_cmd', { limit: -1 });
      assert.ok(logs.every(log => log.source_path.startsWith(run)), 'Expected only test-owned history');
      await button('History').click();
      const filename = page.getByLabel('Filename', { exact: true });
      await filename.fill('faktura B.mzt'); await button('Filter history').click();
      const row = page.locator('label').filter({ hasText: 'faktura B.mzt' });
      const checkbox = row.getByRole('checkbox');
      await until(() => checkbox.isEnabled(), 'Expected a restorable history row');
      await filename.fill('unapplied search');
      await button('Revert All').click(); await button('Restore all actions').click();
      await row.getByText(/· Restored$/).waitFor();
      assert.equal(await checkbox.isDisabled(), true);
      assert.equal(await readFile(b, 'utf8'), 'changed since preview');
      assert.equal(await filename.inputValue(), 'unapplied search');
      await button('Clear History').click(); await button('Clear history').click();
      await page.getByText('No history matches these filters.', { exact: true }).waitFor();
      assert.equal(await row.count(), 0);
    });
  }
  let onlyNewId; const second = join(run, 'tylko nowe'); await mkdir(second); const old = join(second, 'stary.mzt'); await writeFile(old, 'old fixture');
  await caseOf('Only-new baseline configured through the folder form', async () => {
    await button('Watched Folders').click(); await page.getByPlaceholder('C:/Users/.../Downloads').fill(second);
    await page.getByLabel('Process only new files', { exact: true }).first().check(); await button('Add Folder').click();
    await until(async () => { const f = (await invoke('get_folders_cmd')).find(f => f.path === second); if (f) { onlyNewId = f.id; ownedFolders.add(f.id); return f.only_new; } return false; }, 'Only-new was not saved');
    await writeFile(join(second, 'nowy.mzt'), 'new fixture');
    await review([old, join(second, 'nowy.mzt')]); assert.equal(await page.locator('section label').count(), 1);
    await page.getByText(join(second, 'nowy.mzt'), { exact: true }).waitFor();
  });
  await caseOf('Real restart preserves only-new state and saved rules', async () => {
    await stop(); await start(); await button('Review queue').waitFor();
    assert.equal((await invoke('get_folders_cmd')).find(f => f.id === onlyNewId).only_new, true);
    assert.equal((await invoke('get_rules_cmd')).find(r => r.name === ruleName).action, 'rename');
    await review([old, join(second, 'nowy.mzt')]); assert.equal(await page.locator('section label').count(), 1);
    assert.equal(await exists(old), true);
  });
  await caseOf('Polish translation and dark theme in the real application', async () => {
    await button('General').click();
    await page.getByText('Language', { exact: true }).locator('..').locator('select').selectOption('pl');
    await button('Do zatwierdzenia').waitFor();
    await until(async () => (await invoke('get_settings_cmd')).language === 'pl', 'Language change was not saved');
    const selects = page.locator('select'); await selects.nth(1).selectOption('dark');
    await until(() => page.locator('html').evaluate(node => node.classList.contains('dark')), 'Dark theme did not apply');
    await button('Do zatwierdzenia').click(); await page.screenshot({ path: join(run, '04-polish-dark.png'), animations: 'disabled' });
  });
  await caseOf('Warning contrast: app dark theme with system light theme', async () => {
    await page.emulateMedia({ colorScheme: 'light' });
    const warningText = page.getByText('Nazwa sugeruje inny typ pliku', { exact: false });
    await warningText.waitFor();
    report.contrast = await measureContrast(warningText);
    assert.ok(report.contrast.ratio >= 4.5, `Warning contrast is ${report.contrast.ratio.toFixed(2)}:1 (expected at least 4.5:1 for this small text)`);
  });
  await caseOf('Warning and error contrast in all four app/system theme combinations', async () => {
    report.themeMatrix = [];
    for (const system of ['light', 'dark']) for (const app of ['light', 'dark']) {
      await page.emulateMedia({ colorScheme: system });
      await button(pl.settings.general.title).click(); await page.locator('select').nth(1).selectOption(app);
      await until(() => page.locator('html').evaluate((node, dark) => node.classList.contains('dark') === dark, app === 'dark'), 'App theme not applied');
      await button('Do zatwierdzenia').click();
      const warningText = page.getByText('Nazwa sugeruje inny typ pliku', { exact: false }); await warningText.waitFor();
      const warning = await measureContrast(warningText);
      assert.equal(warning.appDark, app === 'dark'); assert.equal(warning.systemDark, system === 'dark');
      assert.ok(warning.ratio >= 4.5, `Warning ${app}/${system}: ${warning.ratio}`);
      await button(pl.settings.rules.title).click(); await button(pl.settings.rules.add).click(); await button(pl.settings.rules.save).click();
      const alert = page.getByRole('alert'); await alert.waitFor();
      const error = await measureContrast(alert); assert.ok(error.ratio >= 4.5, `Error ${app}/${system}: ${error.ratio}`);
      report.themeMatrix.push({ system, app, warning, error });
      await button(pl.common.cancel).click();
    }
    await button('Do zatwierdzenia').click();
    await page.screenshot({ path: join(run, '05-contrast-fixed.png'), animations: 'disabled' });
  });
  assert.deepEqual(report.errors, [], 'Unhandled renderer errors');
} catch (error) { report.failure = String(error); console.error(error); process.exitCode = 1; }
finally {
  if (page && !page.isClosed()) {
    for (const id of ownedFolders) { try { await invoke('remove_folder_cmd', { id }); report.cleanup.push(`Removed test folder ${id}`); } catch (e) { report.cleanup.push(String(e)); } }
    for (const id of ownedRules) { try { await invoke('delete_rule_cmd', { id }); report.cleanup.push(`Removed test rule ${id}`); } catch (e) { report.cleanup.push(String(e)); } }
    if (originalSettings) { try { await invoke('update_settings_cmd', { settings: originalSettings }); report.cleanup.push('Original settings restored'); } catch(e) { report.cleanup.push(String(e)); } }
    if (originalLogs?.length === 0) {
      const logs = await invoke('get_logs_cmd', { limit: -1 });
      if (logs.every(log => log.source_path.startsWith(run))) { await invoke('clear_logs_cmd'); report.cleanup.push('Removed test-only history'); }
    }
    if (originalSettings) {
      report.profileRestored = JSON.stringify(await invoke('get_settings_cmd')) === JSON.stringify(originalSettings)
        && JSON.stringify(await invoke('get_rules_cmd')) === JSON.stringify(originalRules)
        && JSON.stringify(await invoke('get_folders_cmd')) === JSON.stringify(originalFolders);
    }
  }
  await stop().catch(error => report.cleanup.push(String(error)));
  if (ciPolicyInstalled) {
    try { execFileSync('reg.exe', ['delete', ciPolicyKey, '/v', basename(exe), '/f']); report.cleanup.push('Removed application-specific CI debugging policy'); }
    catch (error) { report.cleanup.push(String(error)); process.exitCode = 1; }
  }
  report.finished = new Date().toISOString(); await writeFile(join(run, 'report.json'), JSON.stringify(report, null, 2));
  console.log(`REPORT ${join(run, 'report.json')}`);
}
