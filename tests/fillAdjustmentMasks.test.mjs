import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inflateSync } from 'node:zlib';
import { syncFillAdjustmentMasks } from '../src/utils/fillAdjustmentMasks.ts';

function fixture() {
  return {
    aiPatches: [{ id: 'fill', visible: true, patchData: { mask: 'new-footprint' } }],
    masks: [{ id: 'adjustment', sourceAiPatchId: 'fill', sourceAiSubMaskId: 'region',
      visible: true, adjustments: { exposure: 0.4, saturation: -12 },
      subMasks: [{ id: 'region', visible: true, parameters: { maskDataBase64: 'old-footprint', feather: 2 } }],
    }],
    rotation: 0, flipHorizontal: true, flipVertical: false, orientationSteps: 1,
  };
}

test('updates footprint and orientation without losing manual adjustments', () => {
  const before = fixture();
  const after = syncFillAdjustmentMasks(before);
  assert.equal(after.masks[0].adjustments, before.masks[0].adjustments);
  assert.deepEqual(after.masks[0].subMasks[0].parameters, {
    maskDataBase64: 'new-footprint', feather: 2, rotation: 0,
    flipHorizontal: true, flipVertical: false, orientationSteps: 1,
  });
  assert.equal(before.masks[0].subMasks[0].parameters.maskDataBase64, 'old-footprint');
  assert.equal(syncFillAdjustmentMasks(after), after, 'no update loops when unchanged');
});

test('hidden or deleted fills use a genuinely black mask; showing restores coverage', () => {
  const before = fixture();
  before.aiPatches[0].visible = false;
  const hidden = syncFillAdjustmentMasks(before);
  const png = Buffer.from(hidden.masks[0].subMasks[0].parameters.maskDataBase64, 'base64');
  const idat = png.indexOf(Buffer.from('IDAT'));
  const size = png.readUInt32BE(idat - 4);
  const pixels = inflateSync(png.subarray(idat + 4, idat + 4 + size));
  assert.equal(pixels[1], 0, 'grayscale intensity is zero');
  const deleted = syncFillAdjustmentMasks({ ...before, aiPatches: [] });
  assert.equal(deleted.masks[0].subMasks[0].parameters.maskDataBase64, hidden.masks[0].subMasks[0].parameters.maskDataBase64);
  const shown = syncFillAdjustmentMasks({ ...hidden, aiPatches: fixture().aiPatches });
  assert.equal(shown.masks[0].subMasks[0].parameters.maskDataBase64, 'new-footprint');
  assert.equal(shown.masks[0].adjustments, before.masks[0].adjustments);
});

test('ordinary masks and manually added components are untouched', () => {
  const before = fixture();
  const ordinary = { id: 'ordinary', subMasks: [], adjustments: {} };
  const brush = { id: 'brush', visible: false, parameters: { lines: [] } };
  before.masks.push(ordinary);
  before.masks[0].subMasks.push(brush);
  const after = syncFillAdjustmentMasks(before);
  assert.equal(after.masks[1], ordinary);
  assert.equal(after.masks[0].subMasks[1], brush);
});
