import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, writeFile, readFile, access, realpath } from 'node:fs/promises';
import { resolve, join, basename, isAbsolute, relative, sep } from 'node:path';
import { createHash } from 'node:crypto';
const fork = process.argv.includes('--fork-ui')
  ? await (await import('./windows-ui.mjs')).receiveForkUi()
  : null;
const { chromium } = await import(fork ? 'playwright-core' : process.env.MOUZI_PLAYWRIGHT_MODULE || 'playwright');
const pl = JSON.parse(await readFile(new URL('../src/i18n/locales/pl.json', import.meta.url), 'utf8'));
const version = fork
  ? JSON.parse(await readFile(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8')).version
  : process.env.MOUZI_TEST_VERSION || '0.2.0-beta.2';
const stable = !version.includes('beta');
if (!fork && stable && (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true')) throw Error('Stable UI tests require an isolated GitHub Actions runner. Do not use a personal stable profile.');
const exe = fork?.exe ?? resolve(process.env.MOUZI_TEST_EXE || 'artifacts/0.2.0-beta.2/Mouzi-0.2.0-beta.2-windows-x64.exe');
const run = fork?.run ?? resolve(`artifacts/${version}/private-tests/${Date.now()}`);
const files = fork?.fixture ?? run;
const fixture = join(files, 'pliki testowe — Łódź');
const port = fork?.port ?? 19221;
const endpoint = `http://127.0.0.1:${port}`;
await mkdir(fixture, { recursive: true });
const report = { exe, sha256: createHash('sha256').update(await readFile(exe)).digest('hex'), started: new Date().toISOString(), cases: [], errors: [], cleanup: [] };
if (fork) assert.equal(report.sha256, fork.sha256.toLowerCase(), 'The tested executable changed');
report.contract = fork ? 'fork' : stable ? 'direct-stable' : 'private-beta';
let child, browser, page, originalSettings, folderId, originalLogs, originalRules, originalFolders;
let profileReady = false;
const ciPolicyKey = 'HKLM\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments';
let ciPolicyInstalled = false;
const ownedRules = new Set(), ownedFolders = new Set();
const exists = path => access(path).then(() => true, () => false);
const invoke = (cmd, args = {}) => page.evaluate(({ cmd, args }) => window.__TAURI_INTERNALS__.invoke(cmd, args), { cmd, args });
const button = name => page.getByRole('button', { name, exact: true });
const field = name => fork ? page.getByLabel(name, { exact: true }) : page.getByText(name, { exact: true }).locator('..').locator('input').first();
async function until(check, message, timeout = 15000) { const end = Date.now() + timeout; while (Date.now() < end) { if (await check()) return; await new Promise(r => setTimeout(r, 150)); } throw Error(message); }
async function start() {
  const env = fork ? process.env : { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=19221 --remote-debugging-address=127.0.0.1' };
  child = spawn(exe, fork || stable ? ['--add-folder', fixture] : [], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env });
  child.stdout.on('data', chunk => console.log(String(chunk)));
  child.stderr.on('data', chunk => console.error(String(chunk)));
  try {
    await until(async () => { if (child.exitCode !== null) throw Error(`Application exited during startup: ${child.exitCode}`); try { return (await fetch(endpoint + '/json/version', { signal: AbortSignal.timeout(2000) })).ok; } catch { return false; } }, 'Application debug endpoint did not start', stable ? 120000 : 15000);
  } catch (error) {
    if (!fork && stable) {
      try { console.log(execFileSync('powershell.exe', ['-NoProfile', '-File', 'tests/capture-ci-desktop.ps1'], { encoding: 'utf8', env: { ...process.env, MOUZI_CAPTURE_DIR: run, MOUZI_CAPTURE_PID: String(child.pid) } })); } catch (captureError) { console.error(String(captureError)); }
    }
    throw error;
  }
  browser = await chromium.connectOverCDP(endpoint);
  await until(async () => { page = browser.contexts().flatMap(c => c.pages()).find(p => p.url().includes('settings')); return !!page; }, 'Settings window missing');
  page.on('pageerror', error => report.errors.push(error.message));
  await page.waitForFunction(() => !!window.__TAURI_INTERNALS__);
}
async function stop() {
  try { await browser?.close(); }
  finally {
    if (child && child.exitCode === null && child.signalCode === null) child.kill();
    if (child) await until(() => child.exitCode !== null || child.signalCode !== null, 'Test process did not exit');
    if (fork && child) await until(async () => {
      try { await fetch(endpoint + '/json/version', { signal: AbortSignal.timeout(1000) }); return false; }
      catch { return true; }
    }, 'Test WebView listener did not close');
    child = browser = page = null;
  }
}
async function caseOf(name, fn) {
  try { await fn(); report.cases.push({ name, status: 'PASS' }); console.log(`PASS ${name}`); }
  catch (error) { report.cases.push({ name, status: 'FAIL', error: String(error) }); await page?.screenshot({ path: join(run, 'failure.png') }).catch(() => {}); throw error; }
}
async function review(paths, expected = paths) {
  await invoke('show_review_cmd', { paths });
  await page.getByRole('heading', { name: 'Review queue', exact: true }).waitFor();
  const waitForRequestedRows = async () => {
    if (!fork) return;
    const queue = page.getByRole('heading', { name: 'Review queue', exact: true }).locator('xpath=ancestor::section[1]');
    await until(async () => {
      if (await queue.getAttribute('aria-busy') !== 'false' || await queue.getByRole('checkbox').count() !== expected.length) return false;
      for (const source of expected) {
        if (await queue.locator('label').getByText(source, { exact: true }).count() !== 1) return false;
      }
      return true;
    }, 'Preview did not show the requested files');
  };
  await waitForRequestedRows();
  await until(() => button('Refresh preview').isEnabled(), 'Preview remained busy');
  // A new request can arrive while the existing queue is mounted.
  await button('Refresh preview').click();
  await until(() => button('Refresh preview').isEnabled(), 'Preview did not refresh');
  await waitForRequestedRows();
}
async function applySelected(count) {
  await button('Review selected operations').click();
  await button(`Apply ${count} operations`).click();
  await page.getByRole('heading', { name: 'Operation results', exact: true }).waitFor();
}
async function editRule(ruleName) {
  await button('Rules').click();
  if (fork) { await button(`Edit: ${ruleName}`).click(); return; }
  const row = page.getByText(ruleName, { exact: true }).locator('..').locator('..').locator('..');
  await row.getByRole('button', { name: 'Edit', exact: true }).click();
}
async function saveRule(name) {
  await button('Save').click();
  await until(async () => { const rules = await invoke('get_rules_cmd'); const rule = rules.find(r => r.name === name); if (rule) { ownedRules.add(rule.id); return true; } return false; }, 'Rule was not saved');
}
function belongsToFixture(path) {
  const source = path.startsWith('\\\\?\\') ? path.slice(4) : path;
  const fromFixture = relative(files, source);
  return !!fromFixture && fromFixture !== '..' && !fromFixture.startsWith('..' + sep) && !isAbsolute(fromFixture);
}
async function reviewedHistory() {
  const { total, throughId } = await invoke('preview_clear_logs_cmd');
  const history = (await invoke('history_cmd', { filter: {} })).filter(log => log.id > 0 && log.id <= throughId);
  assert.equal(history.length, total, 'Incomplete history snapshot');
  for (const row of history) {
    assert.ok(row.id > 0 && row.id <= throughId && belongsToFixture(row.source_path)
      && (!row.destination_path || belongsToFixture(row.destination_path)), 'History contains an unowned operation');
  }
  return { total, throughId, history };
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
  if (!fork && stable) {
    // Elevated CI runners require machine policy; WebView2 150+ ignores environment overrides.
    execFileSync('reg.exe', ['add', ciPolicyKey, '/v', basename(exe), '/t', 'REG_SZ', '/d', '--remote-debugging-port=19221 --remote-debugging-address=127.0.0.1', '/f']);
    ciPolicyInstalled = true;
  }
  await start();
  originalSettings = await invoke('get_settings_cmd'); originalLogs = await invoke('get_logs_cmd', { limit: 1 });
  originalRules = await invoke('get_rules_cmd'); originalFolders = await invoke('get_folders_cmd');
  if (fork) {
    assert.deepEqual(originalFolders, [], 'The fork requires a new profile with zero watched folders');
    assert.deepEqual(originalLogs, [], 'The fork requires a new empty action history');
  }
  profileReady = true;
  await invoke('update_settings_cmd', { settings: fork ? { language: 'en' } : { ...originalSettings, language: 'en' } });
  if (!fork) await page.reload();
  await button('About').waitFor();
  if (fork) {
    await until(async () => {
      const path = page.getByPlaceholder('C:/Users/.../Downloads');
      return await path.count() === 1 && await path.inputValue() === fixture;
    }, 'Settings did not receive the requested fixture folder');
  }
  await caseOf('Launch the exact portable EXE; About/version and updater availability', async () => {
    assert.equal((await invoke('get_version_cmd'))[0], version);
    await button('About').click(); await page.getByText(version, { exact: true }).waitFor();
    const buttons = page.getByRole('button').filter({ hasText: /Check.*[Uu]pdate/ });
    if (fork) {
      await button('Upstream downloads').waitFor();
      assert.ok(await button('Upstream downloads').isEnabled());
      assert.equal(await buttons.count(), 0);
    } else assert.equal(await buttons.first().isDisabled(), !stable);
    await page.screenshot({ path: join(run, '01-about.png'), animations: 'disabled' });
  });
  await caseOf('Add a folder with spaces and Polish characters through the UI', async () => {
    await button('Watched Folders').click();
    const folderPath = page.getByPlaceholder('C:/Users/.../Downloads');
    if (fork) assert.equal(await folderPath.inputValue(), fixture, 'Settings must retain the requested folder');
    else await folderPath.fill(fixture);
    if (!fork) await page.getByLabel('Mode for the new folder').selectOption('suggest');
    await button('Add Folder').click();
    await until(async () => { const folder = (await invoke('get_folders_cmd')).find(f => f.path === fixture); if (folder) { folderId = folder.id; ownedFolders.add(folder.id); return folder.mode === (fork ? 'manual' : 'suggest'); } return false; }, 'Folder was not added in the expected mode');
    if (fork) {
      const mode = page.getByRole('combobox', { name: fixture + ': Mode', exact: true });
      assert.equal(await mode.inputValue(), 'manual');
      await mode.selectOption('suggest');
      await until(async () => (await invoke('get_folders_cmd')).find(f => f.id === folderId)?.mode === 'suggest', 'Saved folder did not enter Suggest mode');
    }
  });
  const ruleName = `Private UI ${Date.now()}`;
  await caseOf('UI rule editor: invalid regex, invalid size range, normalized extension list', async () => {
    await button('Rules').click(); await button('Add Rule').click();
    await field('Name').fill(ruleName); await page.getByPlaceholder('Extensions', { exact: true }).fill('mzt, ,');
    await field('Destination').fill('Wynik — dokumenty'); await field('Priority').fill('-100');
    if (fork) await page.getByRole('combobox', { name: 'Watched Folders', exact: true }).selectOption(String(folderId));
    await field('Pattern').fill('['); await button('Save').click();
    await page.getByRole('alert').filter({ hasText: fork ? /Invalid filename pattern|regex parse error/i : 'regular expression' }).waitFor();
    await field('Pattern').fill('');
    await page.getByLabel('Minimum size (bytes)', { exact: true }).fill('20');
    await page.getByLabel('Maximum size (bytes)', { exact: true }).fill('10');
    if (fork) assert.ok(await button('Save').isDisabled(), 'Invalid bounds must block saving');
    else await button('Save').click();
    await page.getByRole('alert').filter({ hasText: 'Minimum size' }).waitFor();
    await page.getByLabel('Minimum size (bytes)', { exact: true }).fill('1');
    await page.getByLabel('Maximum size (bytes)', { exact: true }).fill('100');
    await saveRule(ruleName);
    const rule = (await invoke('get_rules_cmd')).find(r => r.name === ruleName); assert.deepEqual(rule.extensions, ['mzt']);
    if (fork) {
      assert.equal(rule.folder_id, folderId);
      assert.deepEqual([rule.options.min_size, rule.options.max_size], [1, 100]);
    }
  });
  const a = join(fixture, 'faktura A.mzt'), b = join(fixture, 'faktura B.mzt'), warning = join(fixture, 'invoice.pdf.exe');
  const warningBytes = 'This is an inert text fixture, never executed.';
  await writeFile(a, 'invoice alpha'); await writeFile(b, 'invoice beta'); await writeFile(warning, warningBytes);
  await caseOf('Suggest queue, unchecked suspicious filename, selection and cancellation', async () => {
    await until(async () => (await invoke('get_pending_files_cmd')).some(([, name]) => name === 'faktura A.mzt'), 'Suggest queue did not see file');
    assert.equal(await exists(a), true); await review([a, b, warning]);
    const labels = page.locator('section label');
    assert.equal(await labels.filter({ hasText: 'invoice.pdf.exe' }).getByRole('checkbox').isChecked(), false);
    await labels.filter({ hasText: 'faktura B.mzt' }).getByRole('checkbox').uncheck();
    await button('Review selected operations').click(); assert.equal(await exists(a), true);
    await button('Cancel').click();
    assert.equal(await readFile(a, 'utf8'), 'invoice alpha');
    assert.equal(await readFile(b, 'utf8'), 'invoice beta');
    assert.equal(await readFile(warning, 'utf8'), warningBytes);
    await page.screenshot({ path: join(run, '02-preview.png'), animations: 'disabled' });
    await applySelected(1); assert.equal(await exists(a), false);
    assert.equal(await readFile(b, 'utf8'), 'invoice beta');
    assert.equal(await readFile(warning, 'utf8'), warningBytes);
    assert.equal(await readFile(join(fixture, 'Wynik — dokumenty', 'faktura A.mzt'), 'utf8'), 'invoice alpha');
  });
  await caseOf('History filter and safe undo with a conflicting original filename', async () => {
    await writeFile(a, 'unrelated conflict'); await button('History').click();
    await page.getByLabel('Filename', { exact: true }).fill('faktura A.mzt'); await button('Filter history').click();
    await button('Select eligible files').click(); await button('Review undo').click(); await button('Restore 1 files').click();
    await page.getByText('Restored under a different name', { exact: true }).waitFor();
    const restoredPath = (await page.getByText(/^Now at /).textContent()).slice('Now at '.length);
    assert.equal(await realpath(restoredPath), await realpath(join(fixture, 'faktura A_0.mzt')));
    assert.equal(await readFile(restoredPath, 'utf8'), 'invoice alpha');
    assert.equal(await exists(join(fixture, 'Wynik — dokumenty', 'faktura A.mzt')), false);
    assert.equal(await readFile(a, 'utf8'), 'unrelated conflict');
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
    const action = fork ? page.getByRole('combobox', { name: 'Action', exact: true }) : page.getByText('Action', { exact: true }).locator('..').locator('select');
    await action.selectOption('rename');
    if (fork) await page.getByRole('combobox', { name: 'Watched Folders', exact: true }).selectOption('0');
    await page.getByLabel('Filename template', { exact: false }).fill('../{stem}'); await button('Save').click();
    await page.getByRole('alert').filter({ hasText: 'template' }).waitFor();
    await page.getByLabel('Filename template', { exact: false }).fill('gotowe-{stem}.{extension}'); await button('Save').click();
    await until(async () => { const rule = (await invoke('get_rules_cmd')).find(r => r.name === ruleName); return rule.action === 'rename' && (!fork || rule.folder_id === 0); }, 'Rename action or scope did not save');
    await review([b]); await applySelected(1);
    const renamed = join(fixture, 'gotowe-faktura B.mzt'); assert.equal(await exists(b), false); assert.equal(await readFile(renamed, 'utf8'), 'changed since preview');
    assert.equal((await invoke('get_pending_files_cmd')).some(([, name]) => name === 'gotowe-faktura B.mzt'), false);
  });
  if (fork || (stable && originalLogs.length === 0)) {
    await caseOf('History refreshes after Restore All and Clear History without losing the applied filter', async () => {
      const { history } = await reviewedHistory();
      assert.deepEqual(history.map(log => log.source_path).sort(), [a, b].sort(), 'Expected exactly the two test-owned moves');
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
      await reviewedHistory();
      await button('Clear History').click(); await button('Clear history').click();
      await page.getByText('No history matches these filters.', { exact: true }).waitFor();
      assert.equal(await row.count(), 0);
    });
  }
  let onlyNewId; const second = join(files, 'tylko nowe'); await mkdir(second); const old = join(second, 'stary.mzt'); await writeFile(old, 'old fixture');
  await caseOf('Only-new baseline configured through the folder form', async () => {
    await button('Watched Folders').click(); await page.getByPlaceholder('C:/Users/.../Downloads').fill(second);
    const onlyNew = fork
      ? page.getByRole('checkbox', { name: 'Process only new files when adding a folder', exact: true })
      : page.getByLabel('Process only new files', { exact: true }).first();
    await onlyNew.check(); await button('Add Folder').click();
    await until(async () => { const f = (await invoke('get_folders_cmd')).find(f => f.path === second); if (f) { onlyNewId = f.id; ownedFolders.add(f.id); return f.only_new; } return false; }, 'Only-new was not saved');
    await writeFile(join(second, 'nowy.mzt'), 'new fixture');
    await review([old, join(second, 'nowy.mzt')], [join(second, 'nowy.mzt')]);
    await page.getByText(join(second, 'nowy.mzt'), { exact: true }).waitFor();
    assert.equal(await page.locator('section label').count(), 1);
  });
  await caseOf('Real restart preserves only-new state and saved rules', async () => {
    await stop(); await start(); await button('Review queue').waitFor();
    assert.equal((await invoke('get_folders_cmd')).find(f => f.id === onlyNewId).only_new, true);
    assert.equal((await invoke('get_rules_cmd')).find(r => r.name === ruleName).action, 'rename');
    await review([old, join(second, 'nowy.mzt')], [join(second, 'nowy.mzt')]);
    if (fork) await page.getByText(join(second, 'nowy.mzt'), { exact: true }).waitFor();
    assert.equal(await page.locator('section label').count(), 1);
    assert.equal(await readFile(old, 'utf8'), 'old fixture');
    assert.equal(await readFile(join(second, 'nowy.mzt'), 'utf8'), 'new fixture');
  });
  await caseOf('Polish translation and dark theme in the real application', async () => {
    await button('General').click();
    const language = fork ? page.getByRole('combobox', { name: 'Language', exact: true }) : page.getByText('Language', { exact: true }).locator('..').locator('select');
    await language.selectOption('pl');
    await button('Do zatwierdzenia').waitFor();
    await until(async () => (await invoke('get_settings_cmd')).language === 'pl', 'Language change was not saved');
    const theme = fork ? page.getByRole('combobox', { name: pl.settings.general.theme, exact: true }) : page.locator('select').nth(1);
    await theme.selectOption('dark');
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
      await button(pl.settings.general.title).click();
      const theme = fork ? page.getByRole('combobox', { name: pl.settings.general.theme, exact: true }) : page.locator('select').nth(1);
      await theme.selectOption(app);
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
  if (profileReady && page && !page.isClosed()) {
    try {
      for (const id of ownedFolders) {
        await invoke('remove_folder_cmd', { id });
        report.cleanup.push(`Removed test folder ${id}`);
      }
      for (const id of ownedRules) {
        await invoke('delete_rule_cmd', { id });
        report.cleanup.push(`Removed test rule ${id}`);
      }
      const settings = fork ? { language: originalSettings.language, theme: originalSettings.theme } : originalSettings;
      await invoke('update_settings_cmd', { settings });
      if (originalLogs.length === 0) {
        try {
          const { total, throughId } = await reviewedHistory();
          if (total) {
            await invoke('clear_logs_cmd', { throughId });
            report.cleanup.push('Removed reviewed test-only history');
          }
          if (fork) assert.equal((await reviewedHistory()).total, 0);
        } catch (error) {
          if (fork) throw error;
          report.cleanup.push(String(error));
        }
      }
      const restored = await invoke('get_settings_cmd');
      if (fork) {
        assert.ok(restored.revision > originalSettings.revision, 'Expected committed preference writes');
        report.settingsRevisions = { initial: originalSettings.revision, restored: restored.revision };
      }
      const expected = fork ? { ...originalSettings, revision: restored.revision } : originalSettings;
      report.profileRestored = JSON.stringify(restored) === JSON.stringify(expected)
        && JSON.stringify(await invoke('get_rules_cmd')) === JSON.stringify(originalRules)
        && JSON.stringify(await invoke('get_folders_cmd')) === JSON.stringify(originalFolders);
      if (fork) assert.ok(report.profileRestored, 'Disposable profile settings, rules or folders differ after cleanup');
    } catch (error) {
      report.cleanup.push(String(error));
      process.exitCode = 1;
    }
  }
  await stop().catch(error => { report.cleanup.push(String(error)); process.exitCode = 1; });
  if (ciPolicyInstalled) {
    try { execFileSync('reg.exe', ['delete', ciPolicyKey, '/v', basename(exe), '/f']); report.cleanup.push('Removed application-specific CI debugging policy'); }
    catch (error) { report.cleanup.push(String(error)); process.exitCode = 1; }
  }
  report.finished = new Date().toISOString(); await writeFile(join(run, 'report.json'), JSON.stringify(report, null, 2));
  console.log(`REPORT ${join(run, 'report.json')}`);
}
