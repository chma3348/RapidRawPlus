import assert from 'node:assert/strict';
import { test } from 'node:test';
import { photoOwnedAdjustments, transferableAdjustments } from '../src/utils/photoOwnedAdjustments.ts';

test('presets transfer the look without transferring or mutating photo interpretation', () => {
  const preset = {
    exposure: 0.3,
    processVersion: 3,
    v3: { saturation: 20 },
    v3Input: { source: 'other' },
    v3Pipeline: { engine: 'other' },
    v3RawRecovery: 'off',
  };
  const target = { v3Input: { source: 'this' }, v3Pipeline: { engine: 'this' }, v3RawRecovery: 'neutral-green-v1' };
  const result = { ...target, ...transferableAdjustments(preset) };
  assert.deepEqual(photoOwnedAdjustments(result), target);
  assert.equal(result.exposure, 0.3);
  assert.equal(result.v3, preset.v3);
  assert.equal(preset.v3Pipeline.engine, 'other');
  assert.deepEqual(Object.keys(transferableAdjustments(preset)).sort(), ['exposure', 'v3']);
});

test('preview retains only the target photo metadata; absent fields are not invented', () => {
  const target = { exposure: 2, v3Pipeline: { engine: 'this' } };
  assert.deepEqual(photoOwnedAdjustments(target), { v3Pipeline: target.v3Pipeline });
  assert.deepEqual(photoOwnedAdjustments({}), {});
  assert.deepEqual(transferableAdjustments({ exposure: 1 }), { exposure: 1 });
});
