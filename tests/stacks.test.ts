import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildStacks, versionLabels } from '../src/utils/stacks';
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

const D = '/photos/';
const list = [
  file(`${D}DSC1.ARW`, { exif: { DateTimeOriginal: '2026:09:01 10:00:00' } }),
  file(`${D}DSC1.ARW?vc=a1b2c3`),
  file(`${D}DSC1_Restored.tiff`, { derived_from: `${D}DSC1.ARW`, derived_kind: 'Restored' }),
  file(`${D}DSC1_Restored_2.tiff`, { derived_from: `${D}DSC1.ARW`, derived_kind: 'Restored' }),
  file(`${D}DSC1_Restored_Upscaled.png`, { derived_from: `${D}DSC1_Restored.tiff`, derived_kind: 'Upscaled' }),
  file(`${D}DSC2.ARW`, { exif: { DateTimeOriginal: '2026:09:01 11:00:00' } }),
  file(`${D}clip.MOV`, { modified: 5 }),
  file(`${D}clip_frame_00012.png`, { derived_from: `${D}clip.MOV`, derived_kind: 'Frame', modified: 99 }),
  file(`${D}clip_frame_00003.png`, { derived_from: `${D}clip.MOV`, derived_kind: 'Frame', modified: 98 }),
];

test('versions, chained versions and virtual copies stack under the original', () => {
  const stacks = buildStacks(list as any);
  const group = stacks.members.get(`${D}DSC1.ARW`)!.map((f) => f.path.slice(D.length));
  assert.deepEqual(group, [
    'DSC1.ARW',
    'DSC1.ARW?vc=a1b2c3',
    'DSC1_Restored.tiff',
    'DSC1_Restored_2.tiff',
    'DSC1_Restored_Upscaled.png',
  ]);
  assert.equal(stacks.rootOf.get(`${D}DSC1_Restored_Upscaled.png`), `${D}DSC1.ARW`);
  assert.equal(stacks.members.has(`${D}DSC2.ARW`), false);
  assert.equal(stacks.frameSource.get(`${D}clip_frame_00012.png`), `${D}clip.MOV`);
  const labels = [...versionLabels(stacks.members.get(`${D}DSC1.ARW`)!).values()];
  assert.deepEqual(labels, ['Original', 'Copy 1', 'Restored', 'Restored 2', 'Upscaled']);
});

test('the library shows one tile per stack and keeps frames right after their clip', () => {
  const sorted = computeSortedLibrary(
    {
      imageList: list,
      imageRatings: {},
      filterCriteria: { rating: 0, rawStatus: 'all', editedStatus: 'all', colors: [] },
      searchCriteria: { tags: [], text: '', mode: 'AND' },
      sortCriteria: { key: 'date_taken', order: 'asc' },
    },
    { appSettings: {}, supportedTypes: null },
  ).map((f) => f.path.slice(D.length));
  // No capture date sorts first (then by modified); frames follow the clip
  // by name even though they are newer than everything else.
  assert.deepEqual(sorted, ['clip.MOV', 'clip_frame_00003.png', 'clip_frame_00012.png', 'DSC1.ARW', 'DSC2.ARW']);
});

test('a stack is shown when only one of its versions passes the filters', () => {
  const edited = list.map((f) => (f.path.endsWith('_Restored_2.tiff') ? { ...f, is_edited: true } : f));
  const sorted = computeSortedLibrary(
    {
      imageList: edited,
      imageRatings: {},
      filterCriteria: { rating: 0, rawStatus: 'all', editedStatus: 'editedOnly', colors: [] },
      searchCriteria: { tags: [], text: '', mode: 'AND' },
      sortCriteria: { key: 'name', order: 'asc' },
    },
    { appSettings: {}, supportedTypes: null },
  ).map((f) => f.path.slice(D.length));
  assert.deepEqual(sorted, ['DSC1.ARW']);
});
