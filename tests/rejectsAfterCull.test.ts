import assert from 'node:assert/strict';
import { test } from 'node:test';

// A stand-in for the app's backend, recording what it's asked to do.
const calls: [string, any][] = [];
const g = globalThis as any;
g.window = g;
g.dispatchEvent = () => true;
g.__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    calls.push([cmd, args]);
    return null;
  },
  transformCallback: () => 0,
};

const { useLibraryStore } = await import('../src/store/useLibraryStore');
const { useSettingsStore } = await import('../src/store/useSettingsStore');
const { afterCulling, rejectsHandlingFor, rememberRejectsHandling, useRejectsPrompt } =
  await import('../src/utils/rejectsAfterCull');

const file = (path: string, flag: string | null = null) => ({
  path,
  flag,
  modified: 0,
  is_edited: false,
  rating: 0,
  tags: null,
  exif: null,
  is_virtual_copy: false,
});

const library = [
  file('/P/Tahiti/a.ARW', 'reject'),
  file('/P/Tahiti/b.ARW', 'pick'),
  file('/P/Lisbon/c.ARW', 'reject'),
  file('/P/Oslo/d.ARW', 'reject'),
];
const settle = () => new Promise((r) => setTimeout(r, 20));

test('each folder follows its own choice, then the choice for all folders', async () => {
  calls.length = 0;
  useLibraryStore.setState({ imageList: library as any });
  useSettingsStore.setState({
    appSettings: {
      rejectsAfterCull: 'ask',
      rejectsFolderChoices: { '/P/Tahiti': 'move', '/P/Oslo': 'keep' },
    } as any,
  });
  afterCulling(library.map((f) => f.path));
  await settle();
  // Tahiti's reject moves into "Tahiti rejects"; Lisbon's is asked about; Oslo's stays hidden.
  assert.deepEqual(
    calls.filter(([c]) => c === 'move_files').map(([, a]) => [a.sourcePaths, a.destinationFolder]),
    [[['/P/Tahiti/a.ARW'], '/P/Tahiti/Tahiti rejects']],
  );
  assert.deepEqual(useRejectsPrompt.getState().paths, ['/P/Lisbon/c.ARW']);
  useRejectsPrompt.getState().close();
});

test('nothing happens when nothing in the cull was rejected', () => {
  useLibraryStore.setState({ imageList: library as any });
  afterCulling(['/P/Tahiti/b.ARW']);
  assert.equal(useRejectsPrompt.getState().paths, null);
});

test('remembering for a folder, and for all folders', () => {
  useSettingsStore.setState({ appSettings: { rejectsAfterCull: 'ask', rejectsFolderChoices: {} } as any });
  rememberRejectsHandling('keep', 'folders', ['/P/Lisbon']);
  let s = useSettingsStore.getState().appSettings as any;
  assert.equal(rejectsHandlingFor(s, '/P/Lisbon'), 'keep');
  assert.equal(rejectsHandlingFor(s, '/P/Tahiti'), 'ask');

  // Choosing for all folders becomes the general choice and clears these folders' own.
  rememberRejectsHandling('move', 'all', ['/P/Lisbon']);
  s = useSettingsStore.getState().appSettings as any;
  assert.equal(s.rejectsAfterCull, 'move');
  assert.deepEqual(s.rejectsFolderChoices, {});
  assert.equal(rejectsHandlingFor(s, '/P/Lisbon'), 'move');

  // A folder set to the same as all folders keeps no choice of its own.
  rememberRejectsHandling('move', 'folders', ['/P/Oslo']);
  assert.deepEqual((useSettingsStore.getState().appSettings as any).rejectsFolderChoices, {});
});
