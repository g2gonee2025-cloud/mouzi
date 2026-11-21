import assert from 'node:assert/strict';
import { readdir } from 'node:fs/promises';
import test from 'node:test';

test('upstream packaging workflows stay outside the active workflows of this unsigned fork', async () => {
  const upstreamPackaging = new Set([
    'build', 'build-macos-test', 'rebuild-release-assets', 'signpath-test', 'test-appimage',
  ]);
  const active = await readdir(new URL('../.github/workflows/', import.meta.url));
  const restored = active.filter(name => /\.ya?ml$/i.test(name)
    && upstreamPackaging.has(name.replace(/\.ya?ml$/i, '').toLowerCase()));
  assert.deepEqual(restored.sort(), [], 'An upstream merge reactivated a packaging workflow');
});
