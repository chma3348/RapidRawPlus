import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isRejectsFolder, movableRejects, rejectsFolderFor } from '../src/utils/flags';
import { computeSortedLibrary } from '../src/hooks/useSortedLibrary';

const file = (path: string, extra: Record<string, unknown> = {}) => ({
  path,
  modified: 0,
  is_edited: false,
  rating: 0,
  tags: null,
  exif: null,
  is_virtual_copy: path.includes('?vc='),
  ...extra,
});

test('a folder’s rejects go in a subfolder named after it', () => {
  assert.equal(rejectsFolderFor('/Photos/Tahiti'), '/Photos/Tahiti/Tahiti rejects');
  assert.equal(rejectsFolderFor('C:\\Photos\\Tahiti'), 'C:\\Photos\\Tahiti\\Tahiti rejects');
});

test('rejects folders are recognised by name', () => {
  assert.ok(isRejectsFolder('/Photos/Tahiti/Tahiti rejects'));
  assert.ok(isRejectsFolder('/Photos/Tahiti/Rejects'));
  assert.ok(isRejectsFolder('/Photos/Tahiti/Rejected'));
  assert.ok(!isRejectsFolder('/Photos/Tahiti'));
  assert.ok(!isRejectsFolder('/Photos/Projects'));
  assert.ok(!isRejectsFolder(null));
});

test('a photo stays put while any virtual copy of it is a keeper', () => {
  const list = [
    file('/T/a.ARW', { flag: 'reject' }),
    file('/T/b.ARW', { flag: 'reject' }),
    file('/T/b.ARW?vc=1', { flag: null }),
    file('/T/c.ARW', { flag: 'pick' }),
    file('/T/d.ARW?vc=2', { flag: 'reject' }),
    file('/T/T rejects/e.ARW', { flag: 'reject' }),
  ];
  // b has a kept copy; d is only a copy; e is already in a rejects folder.
  assert.deepEqual(movableRejects(list as any), ['/T/a.ARW']);
});

const sorted = (list: any[], extra: Record<string, unknown> = {}) =>
  computeSortedLibrary(
    {
      imageList: list,
      imageRatings: {},
      filterCriteria: { colors: [], rating: 0, rawStatus: 'all' },
      searchCriteria: { tags: [], text: '', mode: 'OR' },
      sortCriteria: { key: 'name', order: 'asc' },
      showRejected: false,
      justRejected: [],
      currentFolderPath: '/T',
      ...extra,
    },
    { appSettings: null, supportedTypes: null },
  ).map((f) => f.path);

test('rejects are hidden unless shown, filtered for, just rejected, or in a rejects folder', () => {
  const list = [file('/T/a.ARW'), file('/T/b.ARW', { flag: 'reject' }), file('/T/c.ARW', { flag: 'pick' })];
  assert.deepEqual(sorted(list), ['/T/a.ARW', '/T/c.ARW']);
  assert.deepEqual(sorted(list, { showRejected: true }), ['/T/a.ARW', '/T/b.ARW', '/T/c.ARW']);
  assert.deepEqual(sorted(list, { justRejected: ['/T/b.ARW'] }), ['/T/a.ARW', '/T/b.ARW', '/T/c.ARW']);
  assert.deepEqual(sorted(list, { filterCriteria: { colors: [], rating: 0, rawStatus: 'all', flag: 'rejected' } }), [
    '/T/b.ARW',
  ]);
  assert.deepEqual(sorted(list, { currentFolderPath: '/T/T rejects' }), ['/T/a.ARW', '/T/b.ARW', '/T/c.ARW']);
});
