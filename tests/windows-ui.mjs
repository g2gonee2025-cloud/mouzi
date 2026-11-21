import assert from 'node:assert/strict';
import { execFileSync, fork } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { constants } from 'node:fs';
import { copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir, userInfo } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = fileURLToPath(import.meta.url);
const repo = dirname(dirname(here));
const target = join(repo, 'src-tauri', 'target');
const prefix = 'mouzi-windows-ui-';
const privateDirs = ['fixture', 'temp', 'webview', 'profile', 'roaming', 'local'];
const isSame = (a, b) => relative(a, b) === '';
const exePath = release => join(target, release ? 'release' : 'debug', 'mouzi.exe');
const systemTool = name => join(process.env.SystemRoot, 'System32', name);
const browserArgs = port => '--remote-debugging-address=127.0.0.1 --remote-debugging-port=' + port + ' --disable-background-networking';
const missing = p => lstat(p).then(() => false, e => { if (e.code === 'ENOENT') return true; throw e; });

function checkIdle() {
  const list = execFileSync(systemTool('tasklist.exe'), ['/fi', 'IMAGENAME eq mouzi.exe', '/fo', 'csv', '/nh'], { encoding: 'utf8', windowsHide: true });
  assert(!/^"mouzi\.exe",/im.test(list), 'Close Mouzi before running its Windows UI tests.');
}

function debugBuild() {
  const env = { ...process.env };
  for (const key of Object.keys(env)) {
    if (/^(TAURI_CONFIG|CARGO_(BUILD_TARGET(_DIR)?|TARGET_DIR|PROFILE_DEV_DEBUG_ASSERTIONS|ENCODED_RUSTFLAGS)|RUSTFLAGS)$/i.test(key)) delete env[key];
  }
  Object.assign(env, { CARGO_TARGET_DIR: target, CARGO_PROFILE_DEV_DEBUG_ASSERTIONS: 'true' });
  const cli = join(repo, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
  execFileSync(process.execPath, [cli, 'build', '--debug', '--no-bundle', '--ci', '--', '--locked', '--jobs', '2'], { cwd: repo, env, stdio: 'inherit', windowsHide: true });
}

async function identify(release) {
  const exe = exePath(release);
  assert(isSame(await realpath(exe), exe), 'Expected the regular executable in this checkout’s Cargo target.');
  const data = await readFile(exe);
  assert.equal(data.subarray(0, 2).toString(), 'MZ', 'Expected a Windows executable.');
  // Only the debug_assertions build includes the development identity and profile guard.
  for (const marker of ['cc.mouzi.dev', 'Development data must be outside the production profile']) {
    assert.equal(data.includes(Buffer.from(marker)), !release, 'Executable profile does not match the requested build.');
  }
  return { exe, sha256: createHash('sha256').update(data).digest('hex') };
}

function powershell(code) {
  return execFileSync(systemTool('WindowsPowerShell\\v1.0\\powershell.exe'), ['-NoProfile', '-NonInteractive', '-Command', "$ErrorActionPreference='Stop';" + code], { encoding: 'utf8', windowsHide: true }).trim();
}

async function hostedProfile() {
  const e = process.env;
  assert(e.GITHUB_ACTIONS === 'true' && e.CI === 'true' && e.RUNNER_ENVIRONMENT === 'github-hosted' && e.RUNNER_OS === 'Windows' && /^\d+$/.test(e.GITHUB_RUN_ID) && userInfo().username.toLowerCase() === 'runneradmin', 'Release UI tests require a fresh GitHub-hosted Windows runner.');
  const slug = e.GITHUB_REPOSITORY;
  assert(/^[\w.-]+\/[\w.-]+$/.test(slug), 'Missing GitHub repository identity.');
  const origin = execFileSync('git', ['-C', repo, 'remote', 'get-url', 'origin'], { encoding: 'utf8', windowsHide: true }).trim().replace(/\.git$/i, '');
  assert.equal(origin.toLowerCase(), ('https://github.com/' + slug).toLowerCase(), 'Checkout origin differs from the Actions repository.');
  const checkout = await realpath(e.GITHUB_WORKSPACE);
  const runnerRoot = dirname(await realpath(e.RUNNER_TEMP));
  const name = slug.split('/')[1];
  assert(isSame(checkout, repo) && isSame(checkout, join(runnerRoot, name, name)), 'Expected the hosted runner’s default checkout.');
  const [roaming, local] = powershell("[Environment]::GetFolderPath('ApplicationData'); [Environment]::GetFolderPath('LocalApplicationData')").split(/\r?\n/);
  assert(isSame(roaming, e.APPDATA) && isSame(local, e.LOCALAPPDATA), 'Expected the runner account’s unmodified AppData folders.');
  const dirs = [join(roaming, 'mouzi', 'mouzi'), join(local, 'cc.mouzi.app')];
  for (const dir of dirs) assert(await missing(dir), 'Release UI tests require an unused application profile: ' + dir);
  return dirs;
}

// Elevated hosted runners require an application-specific WebView2 policy. Never overwrite an existing value.
function installHostedPolicy(port) {
  const key = 'HKLM\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments';
  const read = () => powershell("$k=[Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments'); if ($k -and $k.GetValueNames() -contains 'mouzi.exe') {'Value:' + $k.GetValue('mouzi.exe')} else {'Absent'}");
  const args = browserArgs(port);
  assert.equal(read(), 'Absent', 'A Mouzi WebView2 policy is already present.');
  const reg = (...params) => execFileSync(systemTool('reg.exe'), params, { windowsHide: true });
  reg('add', key, '/v', 'mouzi.exe', '/t', 'REG_SZ', '/d', args, '/f');
  return () => {
    assert.equal(read(), 'Value:' + args, 'The installed WebView2 policy changed during testing.');
    reg('delete', key, '/v', 'mouzi.exe', '/f');
  };
}

async function checkPort(port = 0) {
  const server = createServer();
  server.listen({ host: '127.0.0.1', port, exclusive: true });
  await once(server, 'listening');
  const assigned = server.address().port;
  await new Promise((done, fail) => server.close(error => error ? fail(error) : done()));
  return assigned;
}

// The shared scenario script asks for this handoff only in its explicit --fork-ui mode.
export async function receiveForkUi() {
  assert(process.platform === 'win32' && process.send && process.connected, 'Start fork UI tests with npm run test:windows-ui.');
  const pending = once(process, 'message');
  process.send('ready');
  const [info] = await pending;
  process.disconnect();
  assert(typeof info.release === 'boolean' && Number.isInteger(info.port) && info.port > 0 && info.port <= 65535);
  assert(isSame(info.run, process.cwd()) && basename(info.run).startsWith(prefix));
  assert(isSame(info.fixture, join(info.run, 'fixture')) && isSame(await realpath(info.fixture), info.fixture));
  if (info.release) await hostedProfile();
  else assert(isSame(process.env.MOUZI_DATA_DIR, join(info.run, 'profile')));
  const identified = await identify(info.release);
  assert(isSame(info.exe, identified.exe) && info.sha256 === identified.sha256);
  checkIdle();
  return info;
}

async function scenarios(info) {
  const env = { ...process.env };
  for (const key of Object.keys(env)) if (/^(MOUZI_|WEBVIEW2_|NODE_OPTIONS$|NODE_PATH$)/i.test(key)) delete env[key];
  env.WEBVIEW2_USER_DATA_FOLDER = join(info.run, 'webview');
  env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = browserArgs(info.port);
  env.TEMP = env.TMP = join(info.run, 'temp');
  if (!info.release) Object.assign(env, { MOUZI_OPEN: 'dashboard', MOUZI_DATA_DIR: join(info.run, 'profile'), APPDATA: join(info.run, 'roaming'), LOCALAPPDATA: join(info.run, 'local') });
  const child = fork(join(repo, 'tests', 'private-beta-ui.mjs'), ['--fork-ui'], { cwd: info.run, env, execArgv: [], windowsHide: true, stdio: ['inherit', 'inherit', 'inherit', 'ipc'] });
  child.once('message', message => { if (message === 'ready') child.send(info); });
  const [code] = await once(child, 'exit');
  assert.equal(code, 0, 'Windows UI scenarios failed.');
}

async function main(args) {
  let release = false, report;
  while (args.length) {
    const option = args.shift();
    if (option === '--ci-release' && !release) release = true;
    else if (option === '--report' && !report && args[0] && !args[0].startsWith('--')) report = resolve(args.shift());
    else throw Error('Usage: npm run test:windows-ui -- [--report <new-file>] [--ci-release]');
  }
  assert.equal(process.platform, 'win32', 'Windows UI tests require Windows.');
  if (report) assert(await missing(report), 'The requested report already exists.');
  const profileDirs = release ? await hostedProfile() : [];
  checkIdle();
  if (!release) debugBuild();
  const artifact = await identify(release);
  checkIdle();
  const temp = await realpath(release ? process.env.RUNNER_TEMP : tmpdir());
  const run = await mkdtemp(join(temp, prefix));
  console.log('Windows UI evidence: ' + run);
  const receipt = join(run, 'report.json');
  let restorePolicy, port, passed = false;
  try {
    const fixture = join(run, 'fixture');
    for (const dir of privateDirs) await mkdir(join(run, dir));
    port = await checkPort();
    if (release) restorePolicy = installHostedPolicy(port);
    await scenarios({ ...artifact, run, fixture, port, release });
    const out = JSON.parse(await readFile(receipt, 'utf8'));
    assert.equal(out.sha256.toLowerCase(), artifact.sha256);
    assert.deepEqual(out.launchHashes, [artifact.sha256, artifact.sha256], 'Both launches must use the verified build.');
    assert.equal((await identify(release)).sha256, artifact.sha256, 'The executable changed during testing.');
    assert.equal(out.contract, 'fork');
    assert.equal(out.profileRestored, true);
    assert(!out.failure && out.errors.length === 0 && out.cases.length === 13 && new Set(out.cases.map(c => c.name)).size === 13 && out.cases.every(c => c.status === 'PASS'), 'Expected all 13 fork scenarios to pass.');
    passed = true;
  } finally {
    try {
      restorePolicy?.();
      if (report && !await missing(receipt)) {
        await mkdir(dirname(report), { recursive: true });
        await copyFile(receipt, report, constants.COPYFILE_EXCL);
        console.log('Windows UI receipt: ' + report);
      }
    } finally {
      checkIdle();
      if (port) await checkPort(port);
      assert(isSame(run, await realpath(run)) && isSame(dirname(run), temp));
      const owned = passed ? [run] : privateDirs.map(dir => join(run, dir));
      for (const dir of [...profileDirs, ...owned]) {
        if (await missing(dir)) continue;
        assert(isSame(dir, await realpath(dir)), 'Unexpected directory at fixture teardown.');
        await rm(dir, { recursive: true, maxRetries: 6, retryDelay: 200 });
      }
      if (!passed && !(await readdir(run)).length) await rm(run, { recursive: true });
    }
  }
  console.log('All 13 Windows UI scenarios passed for SHA-256 ' + artifact.sha256);
}

if (process.argv[1] && isSame(here, resolve(process.argv[1]))) {
  main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
}
