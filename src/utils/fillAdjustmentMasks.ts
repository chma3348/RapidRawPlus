import type { Adjustments } from './adjustments';

// A black 1x1 grayscale PNG: resized by the normal AI-mask path when a fill
// is hidden/deleted. Keep the user's adjustments and component visibility.
const EMPTY_MASK = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';

export function syncFillAdjustmentMasks(adjustments: Adjustments): Adjustments {
  let changed = false;
  const masks = (adjustments.masks || []).map((mask) => {
    if (!mask.sourceAiPatchId || !mask.sourceAiSubMaskId) return mask;
    const patch = adjustments.aiPatches?.find((p) => p.id === mask.sourceAiPatchId);
    const bitmap = patch?.visible !== false && patch?.patchData?.mask ? patch.patchData.mask : EMPTY_MASK;
    const subMasks = mask.subMasks.map((sm) => {
      if (sm.id !== mask.sourceAiSubMaskId) return sm;
      const parameters = sm.parameters as any;
      const next = {
        maskDataBase64: bitmap,
        rotation: adjustments.rotation ?? 0,
        flipHorizontal: !!adjustments.flipHorizontal,
        flipVertical: !!adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps ?? 0,
      };
      if (Object.entries(next).every(([key, value]) => parameters?.[key] === value)) return sm;
      changed = true;
      return { ...sm, parameters: { ...parameters, ...next } };
    });
    return subMasks.every((sm, i) => sm === mask.subMasks[i]) ? mask : { ...mask, subMasks };
  });
  return changed ? { ...adjustments, masks } : adjustments;
}
