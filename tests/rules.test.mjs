import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeExtensions } from '../src/utils/ruleInput.ts';
import { nextDestination } from '../src/utils/rulePath.ts';

test('extension input discards empty comma-separated entries before saving (#63)', () => {
  assert.deepEqual(normalizeExtensions(['exe, msi,']), ['exe', 'msi']);
  assert.deepEqual(normalizeExtensions(['', ' .EXE ', ',msi,,', 'exe']), ['exe', 'msi']);
  assert.deepEqual(normalizeExtensions([',, ']), []);
  assert.deepEqual(normalizeExtensions(['*', '']), ['*']);
});
test('folder picker preserves relative paths, respects boundaries and cancellation (#66)', () => {
  assert.equal(nextDestination('C:\\Users\\Test\\Downloads\\Images', ['c:/users/test/downloads/']), 'Images');
  assert.equal(nextDestination('C:\\Downloads-old\\Images', ['C:\\Downloads']), 'C:\\Downloads-old\\Images');
  assert.equal(nextDestination('/home/user/downloads/images', ['/home/user/downloads']), 'images');
  assert.equal(nextDestination('/home/user/Downloads/images', ['/home/user/downloads']), '/home/user/Downloads/images');
  assert.equal(nextDestination('/downloads/work/images', ['/downloads', '/downloads/work']), 'images');
  assert.equal(nextDestination('/downloads/work/images', ['/downloads', '/downloads/work'], '/downloads'), 'work/images');
  assert.equal(nextDestination('/downloads/work/images', ['/downloads', '/downloads/work'], null), '/downloads/work/images');
  assert.equal(nextDestination('/downloads', ['/downloads']), '/downloads');
  assert.equal(nextDestination('/downloads/work', ['/downloads', '/downloads/work']), '/downloads/work');
  assert.equal(nextDestination('/downloads/work', ['/downloads', '/downloads/work'], '/downloads'), '/downloads/work');
  assert.equal(nextDestination('/images', ['/']), 'images');
  assert.equal(nextDestination(null, ['/downloads']), null);
  assert.equal(nextDestination([], ['/downloads']), null);
});
