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
  dehaze: number;
}
export const defaultV3Detail = (): V3Detail => ({
  sharpening: 0,
  threshold: 15,
  texture: 0,
  clarity: 0,
  structure: 0,
  luminance_noise: 0,
  color_noise: 0,
  dehaze: 0,
});

/** Vignette, grain, glow, halation, flare and chromatic aberration, with the previous engine's slider meanings. */
export interface V3Effects {
  vignette_amount: number;
  vignette_midpoint: number;
  vignette_roundness: number;
  vignette_feather: number;
  grain_amount: number;
  grain_size: number;
  grain_roughness: number;
  glow_amount: number;
  halation_amount: number;
  flare_amount: number;
  ca_red_cyan: number;
  ca_blue_yellow: number;
  centre: number;
  film_saturation: number;
}
export const defaultV3Effects = (): V3Effects => ({
  vignette_amount: 0,
  vignette_midpoint: 50,
  vignette_roundness: 0,
  vignette_feather: 50,
  grain_amount: 0,
  grain_size: 25,
  grain_roughness: 50,
  glow_amount: 0,
  halation_amount: 0,
  flare_amount: 0,
  ca_red_cyan: 0,
  ca_blue_yellow: 0,
  centre: 0,
  film_saturation: 0,
});

/** The previous engine's camera calibration, -100..100 each. */
export interface V3Calibration {
  shadows_tint: number;
  red_hue: number;
  red_saturation: number;
  green_hue: number;
  green_saturation: number;
  blue_hue: number;
  blue_saturation: number;
}
export const defaultV3Calibration = (): V3Calibration => ({
  shadows_tint: 0,
  red_hue: 0,
  red_saturation: 0,
  green_hue: 0,
  green_saturation: 0,
  blue_hue: 0,
  blue_saturation: 0,
});

/** V3's own settings. The Basic panel's tone controls are the previous
 *  engine's, saved at the top level alongside these. */
export interface V3Controls {
  revision: number;
  temperature: number;
  tint: number;
  saturation: number;
  vibrance: number;
  hue: number;
  bands: number[][];
  grading: number[][];
  curve: number[];
  /** Red, green, blue curves, applied per channel in DaVinci Intermediate. */
  channel_curves: number[][];
  ranges: V3ColorRange[];
  detail: V3Detail;
  effects: V3Effects;
  calibration: V3Calibration;
}
export function defaultV3Controls(): V3Controls {
  return {
    revision: 1,
    temperature: 0,
    tint: 0,
    saturation: 0,
    vibrance: 0,
    hue: 0,
    bands: Array.from({ length: 8 }, () => [0, 0, 0]),
    grading: Array.from({ length: 4 }, () => [0, 0, 0]),
    curve: [0, 0.25, 0.5, 0.75, 1],
    channel_curves: Array.from({ length: 3 }, () => [0, 0.25, 0.5, 0.75, 1]),
    ranges: [],
    detail: defaultV3Detail(),
    effects: defaultV3Effects(),
    calibration: defaultV3Calibration(),
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
    if (key === 'effects') {
      // Only the amounts fade: a half-strength preset keeps the vignette's
      // shape and the grain's size, at half the effect. Chromatic aberration
      // is a correction for the lens, not a look, so it does not fade.
      const from = neutral.effects;
      const to = { ...from, ...(target.effects ?? {}) };
      const fade = (k: keyof V3Effects) => from[k] + (to[k] - from[k]) * fraction;
      result.effects = {
        ...to,
        vignette_amount: fade('vignette_amount'),
        grain_amount: fade('grain_amount'),
        glow_amount: fade('glow_amount'),
        halation_amount: fade('halation_amount'),
        flare_amount: fade('flare_amount'),
        centre: fade('centre'),
        film_saturation: fade('film_saturation'),
      };
    } else if (key === 'calibration') {
      const from = neutral.calibration;
      const to = { ...from, ...(target.calibration ?? {}) };
      result.calibration = Object.fromEntries(
        (Object.keys(from) as (keyof V3Calibration)[]).map((k) => [k, from[k] + (to[k] - from[k]) * fraction]),
      ) as unknown as V3Calibration;
    } else if (key === 'detail') {
      // Every detail value fades toward neutral, the threshold included, so
      // a half-strength preset is a half-strength preset.
      const from = neutral.detail;
      const to = { ...from, ...(target.detail ?? {}) };
      result.detail = Object.fromEntries(
        (Object.keys(from) as (keyof V3Detail)[]).map((k) => [k, from[k] + (to[k] - from[k]) * fraction]),
      ) as unknown as V3Detail;
    } else if (key === 'channel_curves') {
      const target_curves = target.channel_curves ?? neutral.channel_curves;
      result.channel_curves = neutral.channel_curves.map((curve, c) =>
        curve.map((v, i) => v + ((target_curves[c]?.[i] ?? v) - v) * fraction),
      );
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
