import assert from 'node:assert/strict';
import { test } from 'node:test';
import { useEditorStore } from '../src/store/useEditorStore';
import { INITIAL_ADJUSTMENTS, normalizeLoadedAdjustments } from '../src/utils/adjustments';
import { defaultV3Controls, defaultV3Range } from '../src/utils/colorV3';

test('advanced color controls survive editor undo, redo and loaded-edit normalization', () => {
  const initial = { ...INITIAL_ADJUSTMENTS, processVersion: 3, v3: defaultV3Controls() };
  const edited = {
    ...initial,
    v3: {
      ...initial.v3,
      curve: [0, 0.15, 0.45, 0.85, 1],
      ranges: [{ ...defaultV3Range(), adjustment: [20, 35, -10] }],
    },
  };
  useEditorStore.getState().resetHistory(initial);
  useEditorStore.getState().setEditor({ adjustments: edited });
  useEditorStore.getState().pushHistory(edited);
  useEditorStore.getState().undo();
  assert.deepEqual(useEditorStore.getState().adjustments.v3, initial.v3);
  useEditorStore.getState().redo();
  assert.deepEqual(useEditorStore.getState().adjustments.v3, edited.v3);
  const loaded = normalizeLoadedAdjustments(JSON.parse(JSON.stringify(edited)));
  assert.deepEqual(loaded.v3, edited.v3);
  assert.equal(loaded.processVersion, 3);
  assert.deepEqual(initial.v3.curve, [0, 0.25, 0.5, 0.75, 1]);
});

test('pipeline identity survives save/load and is undoable without mutating old edits', () => {
  const initial = { ...INITIAL_ADJUSTMENTS, processVersion: 3, v3: defaultV3Controls() };
  const identity = {
    schema: 1,
    engine: 'v3-stable-input-2',
    input_policy: 'profiled-display-cube-p3-or-compress-1',
    raw_development: 'bayer-d65-green-clipped-neutral-1',
    input_transform: null,
    output_transform: { blake3: 'a'.repeat(64) },
  };
  const pinned = { ...initial, v3Pipeline: identity, v3RawRecovery: 'off' as const };
  useEditorStore.getState().resetHistory(initial);
  useEditorStore.getState().setEditor({ adjustments: pinned });
  useEditorStore.getState().pushHistory(pinned);
  useEditorStore.getState().undo();
  assert.equal(useEditorStore.getState().adjustments.v3Pipeline, undefined);
  useEditorStore.getState().redo();
  assert.deepEqual(useEditorStore.getState().adjustments.v3Pipeline, identity);
  assert.deepEqual(normalizeLoadedAdjustments(JSON.parse(JSON.stringify(pinned))).v3Pipeline, identity);
  assert.equal(normalizeLoadedAdjustments(JSON.parse(JSON.stringify(pinned))).v3RawRecovery, 'off');
  assert.equal(normalizeLoadedAdjustments(initial).v3Pipeline, undefined);
});

test('undo, redo, presets and reopening cannot reactivate a retired engine', () => {
  const old = { ...INITIAL_ADJUSTMENTS, processVersion: 2, highlights: -70, shadows: 80 };
  useEditorStore.getState().resetHistory(old);
  useEditorStore.getState().pushHistory({ ...old, shadows: 25 });
  useEditorStore.getState().goToHistoryIndex(1);
  useEditorStore.getState().undo();
  assert.equal(useEditorStore.getState().adjustments.processVersion, 3);
  assert.equal(useEditorStore.getState().adjustments.highlights, -70);
  useEditorStore.getState().redo();
  assert.equal(useEditorStore.getState().adjustments.processVersion, 3);
  assert.equal(useEditorStore.getState().adjustments.shadows, 25);
  useEditorStore.getState().setEditor({ adjustments: old });
  assert.equal(useEditorStore.getState().adjustments.processVersion, 3);
  assert.equal(normalizeLoadedAdjustments(old).processVersion, 3);
});
