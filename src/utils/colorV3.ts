export interface V3ColorRange {
  center: number[];
  width: number[];
  adjustment: number[];
}
export const defaultV3Range = (): V3ColorRange => ({
  center: [30, 0.12, 0.65],
  width: [45, 0.2, 0.5],
  adjustment: [0, 0, 0],
});
/** Same monotone Hermite interpolation as the shader, for the curve graph. */
export function evaluateV3Curve(curve: number[], x: number): number {
  const d = curve.slice(1).map((v, i) => (v - curve[i]) * 4);
  const m = curve.map((_, i) => (i === 0 ? d[0] : i === 4 ? d[3] : (2 * d[i - 1] * d[i]) / (d[i - 1] + d[i])));
  if (x >= 1) return 1 + (x - 1) * m[4];
  const i = Math.min(3, Math.max(0, Math.floor(x * 4))),
    t = x * 4 - i;
  return (
    (2 * t * t * t - 3 * t * t + 1) * curve[i] +
    ((t * t * t - 2 * t * t + t) * m[i]) / 4 +
    (-2 * t * t * t + 3 * t * t) * curve[i + 1] +
    ((t * t * t - t * t) * m[i + 1]) / 4
  );
}
/** Spatial controls, applied as their own stage before the pointwise pass. */
export interface V3Detail {
  sharpening: number;
  threshold: number;
  texture: number;
  clarity: number;
  structure: number;
  luminance_noise: number;
  color_noise: number;
}
export const defaultV3Detail = (): V3Detail => ({
  sharpening: 0,
  threshold: 15,
  texture: 0,
  clarity: 0,
  structure: 0,
  luminance_noise: 0,
  color_noise: 0,
});

export interface V3Controls {
  revision: number;
  exposure: number;
  temperature: number;
  tint: number;
  contrast: number;
  pivot: number;
  shadows: number;
  highlights: number;
  blacks: number;
  whites: number;
  saturation: number;
  vibrance: number;
  hue: number;
  bands: number[][];
  grading: number[][];
  curve: number[];
  ranges: V3ColorRange[];
  detail: V3Detail;
}
export function defaultV3Controls(): V3Controls {
  return {
    revision: 1,
    exposure: 0,
    temperature: 0,
    tint: 0,
    contrast: 0,
    pivot: 0.18,
    shadows: 0,
    highlights: 0,
    blacks: 0,
    whites: 0,
    saturation: 0,
    vibrance: 0,
    hue: 0,
    bands: Array.from({ length: 8 }, () => [0, 0, 0]),
    grading: Array.from({ length: 4 }, () => [0, 0, 0]),
    curve: [0, 0.25, 0.5, 0.75, 1],
    ranges: [],
    detail: defaultV3Detail(),
  };
}

/** Versions are discrete; only creative parameters participate in intensity. */
export function mixV3Controls(preset: Partial<V3Controls>, intensity: number): V3Controls {
  const neutral = defaultV3Controls();
  const target = { ...neutral, ...preset };
  const fraction = Math.max(0, Math.min(100, intensity)) / 100;
  const result = { ...target };
  for (const key of Object.keys(neutral) as (keyof V3Controls)[]) {
    if (key === 'revision') continue;
    if (key === 'detail') {
      // Every detail value fades toward neutral, the threshold included, so
      // a half-strength preset is a half-strength preset.
      const from = neutral.detail;
      const to = { ...from, ...(target.detail ?? {}) };
      result.detail = Object.fromEntries(
        (Object.keys(from) as (keyof V3Detail)[]).map((k) => [k, from[k] + (to[k] - from[k]) * fraction]),
      ) as unknown as V3Detail;
    } else if (key === 'curve') {
      result.curve = neutral.curve.map((v, i) => v + (target.curve[i] - v) * fraction);
    } else if (key === 'ranges') {
      result.ranges = target.ranges.map((r) => ({
        center: [...r.center],
        width: [...r.width],
        adjustment: r.adjustment.map((v) => v * fraction),
      }));
    } else if (key === 'bands' || key === 'grading') {
      result[key] = neutral[key].map((row, i) =>
        row.map((v, j) => {
          const value = target[key]?.[i]?.[j] ?? v;
          // Wheel hue chooses the tint; amount fades it, not a spin from red.
          return key === 'grading' && j === 0 ? value : v + (value - v) * fraction;
        }),
      );
    } else {
      result[key] = neutral[key] + (target[key] - neutral[key]) * fraction;
    }
  }
  return result;
}
