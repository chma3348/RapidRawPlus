import { INITIAL_ADJUSTMENTS } from './adjustments';
import { mixV3Controls } from './colorV3';
import { PHOTO_OWNED_ADJUSTMENTS, transferableAdjustments } from './photoOwnedAdjustments';

const evaluateCurveY = (curve: Array<{ x: number; y: number }>, targetX: number): number => {
  const len = curve.length;
  if (len === 1) return curve[0].y;
  if (targetX <= curve[0].x) return curve[0].y;
  if (targetX >= curve[len - 1].x) return curve[len - 1].y;

  for (let i = 0; i < len - 1; i++) {
    const p2 = curve[i + 1];
    if (targetX <= p2.x) {
      const p1 = curve[i];
      const range = p2.x - p1.x;
      return range === 0 ? p1.y : p1.y + ((targetX - p1.x) / range) * (p2.y - p1.y);
    }
  }
  return targetX;
};

/**
 * A preset at `intensity` percent: each of its settings moved that far from
 * its default (`initialObj`) toward the preset's value. Only the preset's
 * own settings; spread over the photo's, the rest stay the photo's.
 */
export const mixAdjustments = (presetObj: any, intensity: number, initialObj: any = INITIAL_ADJUSTMENTS): any => {
  const fraction = intensity / 100;

  // A photo's own record, never part of a look: the interpretation snapshot,
  // the pinned rendering identity (asset hashes that exist on this machine)
  // and the RAW source treatment. Copying them to another photo would pin it
  // to assets it may not have, or change how its sensor data is read.
  const PHOTO_OWN: readonly string[] = PHOTO_OWNED_ADJUSTMENTS;
  if (fraction === 1) {
    return transferableAdjustments(presetObj);
  }
  // At 0 only the preset's own settings go back to their defaults. The
  // photo's other edits (crop, masks, fills, rotation, lens) are not the
  // preset's to reset: a whole default set spread over them wiped them.
  if (fraction === 0) {
    const result: Record<string, unknown> = {};
    const defaults = INITIAL_ADJUSTMENTS as unknown as Record<string, unknown>;
    for (const key of Object.keys(presetObj)) {
      if (PHOTO_OWN.includes(key) || ['processVersion', 'v3PreviousVersion', 'v3PreviousToneMapper'].includes(key)) {
        continue;
      } else if (key === 'revision') {
        result[key] = presetObj[key];
      } else if (key === 'v3') {
        result[key] = mixV3Controls(presetObj[key], 0);
      } else {
        result[key] = initialObj[key] !== undefined ? initialObj[key] : defaults[key];
      }
    }
    return result;
  }

  const result: any = {};
  const keys = Object.keys(presetObj);

  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    const presetVal = presetObj[key];
    const initialVal = initialObj[key] !== undefined ? initialObj[key] : (INITIAL_ADJUSTMENTS as any)[key];

    if (PHOTO_OWN.includes(key) || ['processVersion', 'v3PreviousVersion', 'v3PreviousToneMapper'].includes(key)) {
      continue;
    } else if (key === 'revision') {
      result[key] = presetVal;
    } else if (key === 'v3') {
      result[key] = mixV3Controls(presetVal, intensity);
    } else if (typeof presetVal === 'number') {
      result[key] = typeof initialVal === 'number' ? initialVal + (presetVal - initialVal) * fraction : presetVal;
    } else if (Array.isArray(presetVal)) {
      if (!Array.isArray(initialVal)) {
        result[key] = fraction > 0 ? presetVal : initialVal;
        continue;
      }

      if (presetVal.length > 0 && presetVal[0].x !== undefined && presetVal[0].y !== undefined) {
        const xVals: number[] = [];
        let p1 = 0,
          p2 = 0;
        const len1 = initialVal.length,
          len2 = presetVal.length;

        while (p1 < len1 && p2 < len2) {
          const x1 = initialVal[p1].x,
            x2 = presetVal[p2].x;
          if (x1 < x2) {
            xVals.push(x1);
            p1++;
          } else if (x1 > x2) {
            xVals.push(x2);
            p2++;
          } else {
            xVals.push(x1);
            p1++;
            p2++;
          }
        }
        while (p1 < len1) xVals.push(initialVal[p1++].x);
        while (p2 < len2) xVals.push(presetVal[p2++].x);

        const newCurve = new Array(xVals.length);

        for (let j = 0; j < xVals.length; j++) {
          const x = xVals[j];
          const yInit = evaluateCurveY(initialVal, x);
          const yPreset = evaluateCurveY(presetVal, x);
          const yInterp = yInit + (yPreset - yInit) * fraction;

          newCurve[j] = {
            x,
            y: yInterp < 0 ? 0 : yInterp > 255 ? 255 : yInterp,
          };
        }
        result[key] = newCurve;
      } else {
        result[key] = fraction > 0 ? presetVal : initialVal;
      }
    } else if (presetVal !== null && typeof presetVal === 'object') {
      result[key] = mixAdjustments(presetVal, intensity, initialVal || {});
    } else {
      result[key] = fraction > 0 ? presetVal : initialVal;
    }
  }
  return result;
};
