import assert from 'node:assert/strict';
import { test } from 'node:test';
import { currentColorEngine } from '../src/utils/currentColorEngine.ts';

test('development edits adopt v3 while retaining shared tonal and geometry work', () => {
  for (const processVersion of [undefined, 0, 1, 2]) {
    const old = {
      processVersion,
      highlights: -70,
      shadows: 80,
      crop: { x: 3 },
      toneMapper: 'basic',
      v3PreviousVersion: 1,
    };
    const result = currentColorEngine(old);
    assert.equal(result.processVersion, 3);
    assert.equal(result.toneMapper, 'resolve');
    assert.equal(result.highlights, -70);
    assert.equal(result.shadows, 80);
    assert.equal(result.crop, old.crop);
    assert.equal(result.v3PreviousVersion, undefined);
    assert.deepEqual(currentColorEngine(result), result);
    assert.equal(old.processVersion, processVersion);
  }
});

test('old pins adopt current policy without losing their captured assets', () => {
  const old = {
    processVersion: 3,
    toneMapper: 'basic',
    v3Input: { stale: true },
    v3Pipeline: {
      schema: 1,
      engine: 'v3-stable-input-1',
      input_policy: 'profiled-display-cube-or-wide-gamut-bypass-1',
      input_transform: { blake3: 'a'.repeat(64) },
    },
  };
  const result = currentColorEngine(old);
  assert.equal(result.v3Pipeline.engine, 'v3-stable-input-2');
  assert.equal(result.v3Pipeline.input_policy, 'profiled-display-cube-p3-or-compress-1');
  assert.equal(result.v3Pipeline.input_transform, old.v3Pipeline.input_transform);
  assert.equal(result.v3Input, undefined);
  assert.equal(result.toneMapper, 'basic');
});

test('unknown future versions remain identifiable for backend rejection', () => {
  const future = { processVersion: 4 };
  assert.equal(currentColorEngine(future), future);
});
