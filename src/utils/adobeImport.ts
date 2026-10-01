/**
 * Turning Lightroom and Camera Raw develop settings into this app's edits,
 * for people moving over from Adobe. The settings are read from XMP by
 * `read_adobe_develop` (src-tauri/src/adobe_develop.rs); this maps them onto
 * the v3 engine's controls.
 *
 * Engines differ, so this aims for close, not identical: each Adobe slider
 * moves the matching control here by the amount in `ADOBE_TUNING`, which is
 * the one place to adjust when comparing against Lightroom's own renders.
 */
import { INITIAL_ADJUSTMENTS, normalizeLoadedAdjustments } from './adjustments';
import { defaultV3Controls, V3Controls } from './colorV3';

export interface AdobeDevelop {
  values: Record<string, string>;
  /** Point curves on Adobe's 0–255 scale, by name. */
  curves: Record<string, [number, number][]>;
  localCorrections: number;
  retouchSpots: number;
  source: string;
  xmpOrientation?: number | null;
  geometry?: { width: number; height: number; fileOrientation: number } | null;
  asShotXy?: [number, number] | null;
}

/** How far each Adobe slider moves ours. A first pass, to refine against Lightroom exports. */
export const ADOBE_TUNING = {
  /** Our exposure per Adobe stop: the engine applies 2^(value / 0.8). */
  exposure: 0.8,
  contrast: 1,
  highlights: 1,
  shadows: 1,
  whites: 1,
  blacks: 1,
  texture: 1,
  clarity: 1,
  dehaze: 1,
  vibrance: 1,
  saturation: 1,
  /** Colour mixer hue: degrees here per Adobe step (±100 → ±30°). */
  hslHueDegrees: 0.3,
  hslSaturation: 1,
  hslLuminance: 1,
  /** Adobe sharpening runs 0–150 (40 by default on RAWs). */
  sharpening: 0.4,
  luminanceNoise: 1,
  colorNoise: 1,
  /** Distance from the daylight curve (Duv) per Adobe tint step, for RAWs: the DNG SDK's scale of 3000. */
  tintDuv: 1 / 3000,
  /** For JPEG and TIFF, Adobe's temperature and tint are already relative. */
  jpegTemperature: 1,
  jpegTint: 1,
  /** The gamma Adobe's tone curve roughly works in. */
  curveGamma: 2.2,
  /** +1 if Adobe's crop angle turns the same way as ours, −1 if not. */
  angleSign: 1,
};

const T = ADOBE_TUNING;
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const HUES = ['Red', 'Orange', 'Yellow', 'Green', 'Aqua', 'Blue', 'Purple', 'Magenta'];

const reader =
  (d: AdobeDevelop) =>
  (key: string, fallback = 0) => {
    const n = parseFloat(d.values[key] ?? '');
    return Number.isFinite(n) ? n : fallback;
  };

// ---------------------------------------------------------------------------
// White balance
// ---------------------------------------------------------------------------

/** CIE xy of a black body at `k` kelvin (Kim et al. cubic fit, 1667–25000 K). */
export function planckXy(k: number): [number, number] {
  const t = clamp(k, 1667, 25000);
  const x =
    t <= 4000
      ? -0.2661239e9 / t ** 3 - 0.2343589e6 / t ** 2 + 0.8776956e3 / t + 0.17991
      : -3.0258469e9 / t ** 3 + 2.1070379e6 / t ** 2 + 0.2226347e3 / t + 0.24039;
  const y =
    t <= 2222
      ? -1.1063814 * x ** 3 - 1.3481102 * x ** 2 + 2.18555832 * x - 0.20219683
      : t <= 4000
        ? -0.9549476 * x ** 3 - 1.37418593 * x ** 2 + 2.09137015 * x - 0.16748867
        : 3.081758 * x ** 3 - 5.8733867 * x ** 2 + 3.75112997 * x - 0.37001483;
  return [x, y];
}

const xyToUv = ([x, y]: [number, number]) => {
  const d = -2 * x + 12 * y + 3;
  return [(4 * x) / d, (6 * y) / d];
};
const uvToXy = ([u, v]: number[]): [number, number] => {
  const d = 2 * u - 8 * v + 4;
  return [(3 * u) / d, (2 * v) / d];
};

/** The white of a light at `k` kelvin, moved `duv` off the black-body curve (positive is greener). */
export function whiteXy(k: number, duv: number): [number, number] {
  const a = xyToUv(planckXy(k));
  const b = xyToUv(planckXy(k + 10));
  const len = Math.hypot(b[0] - a[0], b[1] - a[1]) || 1;
  // Perpendicular to the curve; the curve runs right-to-left as kelvin rise, so this points to green.
  const n = [(b[1] - a[1]) / len, -(b[0] - a[0]) / len];
  const sign = n[1] > 0 ? 1 : -1;
  return uvToXy([a[0] + sign * n[0] * duv, a[1] + sign * n[1] * duv]);
}

const BRADFORD = [
  [0.8951, 0.2664, -0.1614],
  [-0.7502, 1.7135, 0.0367],
  [0.0389, -0.0685, 1.0296],
];
const lms = ([x, y]: [number, number]) => {
  const xyz = [x / y, 1, (1 - x - y) / y];
  return BRADFORD.map((row) => row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]);
};

/**
 * Our temperature and tint (−100..100) that rebalance a photo from the
 * camera's as-shot white to Adobe's chosen one. The engine scales the long
 * and short cone channels by 2^(±0.006·temperature + 0.006·tint) relative to
 * the middle one, so the shift is solved for directly.
 */
export function whiteBalanceShift(asShot: [number, number], target: [number, number]) {
  const a = lms(asShot);
  const b = lms(target);
  const g = a.map((v, i) => v / b[i]);
  const long = Math.log2(g[0] / g[1]);
  const short = Math.log2(g[2] / g[1]);
  return {
    temperature: clamp((long - short) / 0.012, -100, 100),
    tint: clamp((long + short) / 0.012, -100, 100),
  };
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

/** Monotone cubic through Adobe's curve points (Fritsch–Carlson), as Adobe draws them. */
function curveFn(points: [number, number][]) {
  const p = [...points].sort((a, b) => a[0] - b[0]);
  if (p.length < 2) return (x: number) => x;
  const n = p.length;
  const d = p.slice(1).map((q, i) => (q[1] - p[i][1]) / (q[0] - p[i][0] || 1));
  const m = p.map((_, i) =>
    i === 0 ? d[0] : i === n - 1 ? d[n - 2] : d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2,
  );
  for (let i = 0; i < n - 1; i++) {
    if (d[i] === 0) {
      m[i] = m[i + 1] = 0;
      continue;
    }
    const a = m[i] / d[i];
    const b = m[i + 1] / d[i];
    const s = a * a + b * b;
    if (s > 9) {
      const k = 3 / Math.sqrt(s);
      m[i] = k * a * d[i];
      m[i + 1] = k * b * d[i];
    }
  }
  return (x: number) => {
    if (x <= p[0][0]) return p[0][1];
    if (x >= p[n - 1][0]) return p[n - 1][1];
    let i = 0;
    while (x > p[i + 1][0]) i++;
    const h = p[i + 1][0] - p[i][0];
    const t = (x - p[i][0]) / h;
    return (
      (2 * t ** 3 - 3 * t ** 2 + 1) * p[i][1] +
      (t ** 3 - 2 * t ** 2 + t) * h * m[i] +
      (-2 * t ** 3 + 3 * t ** 2) * p[i + 1][1] +
      (t ** 3 - t ** 2) * h * m[i + 1]
    );
  };
}

const IDENTITY = [0, 0.25, 0.5, 0.75, 1];

/**
 * An Adobe point curve as the engine's five knots. The knots sit at fixed
 * inputs in log2(1 + 16Y) / log2(17); each is taken to Adobe's encoding,
 * through Adobe's curve, and back. The ends stay 0 and 1, and the knots keep
 * at least 0.01 apart, as the engine requires. Null for a straight line.
 */
export function adobeCurveToKnots(points: [number, number][] | undefined): number[] | null {
  if (!points || points.length < 2) return null;
  const f = curveFn(points);
  const log17 = Math.log2(17);
  const knots = IDENTITY.map((x, i) => {
    if (i === 0) return 0;
    if (i === 4) return 1;
    const y = (17 ** x - 1) / 16;
    const out = clamp(f(255 * y ** (1 / T.curveGamma)), 0, 255) / 255;
    return Math.log2(1 + 16 * out ** T.curveGamma) / log17;
  });
  for (let i = 1; i < 4; i++) knots[i] = Math.max(knots[i], knots[i - 1] + 0.01);
  for (let i = 3; i > 0; i--) knots[i] = Math.min(knots[i], knots[i + 1] - 0.01);
  const straight = knots.every((k, i) => Math.abs(k - IDENTITY[i]) < 0.002);
  return straight ? null : knots.map((k) => Math.round(k * 10000) / 10000);
}

// ---------------------------------------------------------------------------
// Orientation and crop
// ---------------------------------------------------------------------------

/** EXIF orientation as quarter turns clockwise and a mirror. */
const ORIENTATION: Record<number, { turns: number; mirror: boolean }> = {
  1: { turns: 0, mirror: false },
  2: { turns: 0, mirror: true },
  3: { turns: 2, mirror: false },
  4: { turns: 2, mirror: true },
  5: { turns: 1, mirror: true },
  6: { turns: 1, mirror: false },
  7: { turns: 3, mirror: true },
  8: { turns: 3, mirror: false },
};

/**
 * Adobe's crop edges are fractions of the photo as stored, before any
 * rotation; this gives them for the photo as shown, upright, for an EXIF
 * orientation. (A portrait photo stored sideways has its crop's top and
 * bottom on its left and right as shown.)
 */
export function cropForOrientation(
  c: { left: number; top: number; right: number; bottom: number },
  orientation: number,
) {
  const { left: L, top: T_, right: R, bottom: B } = c;
  switch (orientation) {
    case 2:
      return { left: 1 - R, right: 1 - L, top: T_, bottom: B };
    case 3:
      return { left: 1 - R, right: 1 - L, top: 1 - B, bottom: 1 - T_ };
    case 4:
      return { left: L, right: R, top: 1 - B, bottom: 1 - T_ };
    case 5:
      return { left: T_, right: B, top: L, bottom: R };
    case 6:
      return { left: 1 - B, right: 1 - T_, top: L, bottom: R };
    case 7:
      return { left: 1 - B, right: 1 - T_, top: 1 - R, bottom: 1 - L };
    case 8:
      return { left: T_, right: B, top: 1 - R, bottom: 1 - L };
    default:
      return { left: L, right: R, top: T_, bottom: B };
  }
}

/**
 * The crop in this app's terms: pixels of the upright photo, after its
 * straightening turns it about its centre. Adobe's crop rectangle turns with
 * the angle about its own centre, so its centre is carried round the photo's
 * centre and its size kept. Null when the crop is the whole photo, or when it
 * reaches outside it (a print layout with borders), which can't be shown here.
 */
export function cropToPixels(
  edges: { left: number; top: number; right: number; bottom: number },
  width: number,
  height: number,
  angleDegrees: number,
) {
  const w = (edges.right - edges.left) * width;
  const h = (edges.bottom - edges.top) * height;
  if (!(w > 1 && h > 1)) return null;
  const outside = edges.left < -0.001 || edges.top < -0.001 || edges.right > 1.001 || edges.bottom > 1.001;
  if (outside) return null;
  const whole = edges.left < 0.001 && edges.top < 0.001 && edges.right > 0.999 && edges.bottom > 0.999;
  if (whole && Math.abs(angleDegrees) < 0.01) return null;
  const cx = ((edges.left + edges.right) / 2) * width - width / 2;
  const cy = ((edges.top + edges.bottom) / 2) * height - height / 2;
  const a = (angleDegrees * Math.PI) / 180;
  const rx = cx * Math.cos(a) - cy * Math.sin(a) + width / 2;
  const ry = cx * Math.sin(a) + cy * Math.cos(a) + height / 2;
  const cw = Math.min(w, width);
  const ch = Math.min(h, height);
  const x = clamp(rx - cw / 2, 0, width - cw);
  const y = clamp(ry - ch / 2, 0, height - ch);
  return { unit: 'px' as const, x: Math.round(x), y: Math.round(y), width: Math.round(cw), height: Math.round(ch) };
}

// ---------------------------------------------------------------------------
// Whole edits
// ---------------------------------------------------------------------------

const TONE_KEYS = [
  'Exposure2012',
  'Contrast2012',
  'Highlights2012',
  'Shadows2012',
  'Whites2012',
  'Blacks2012',
  'Texture',
  'Clarity2012',
  'Dehaze',
  'Vibrance',
  'Saturation',
  'PostCropVignetteAmount',
  'GrainAmount',
  'SplitToningShadowSaturation',
  'SplitToningHighlightSaturation',
  'ColorGradeShadowSat',
  'ColorGradeMidtoneSat',
  'ColorGradeHighlightSat',
  'ColorGradeGlobalSat',
  'ColorGradeShadowLum',
  'ColorGradeMidtoneLum',
  'ColorGradeHighlightLum',
  'ColorGradeGlobalLum',
  ...HUES.flatMap((h) => [`HueAdjustment${h}`, `SaturationAdjustment${h}`, `LuminanceAdjustment${h}`]),
];

/** Whether the photo was actually edited in Adobe, rather than carrying default settings. */
export function hasAdobeEdits(d: AdobeDevelop): boolean {
  const n = reader(d);
  if (TONE_KEYS.some((k) => Math.abs(n(k)) > 0.0001)) return true;
  if ((d.values.WhiteBalance ?? 'As Shot') !== 'As Shot') return true;
  if (d.values.HasCrop === 'True' && edgesOf(d) && Math.abs(n('CropAngle')) + cropArea(d) > 0.0001) return true;
  if (Math.abs(n('CropAngle')) > 0.01) return true;
  if (Object.entries(d.curves).some(([name, pts]) => name.startsWith('ToneCurvePV2012') && adobeCurveToKnots(pts)))
    return true;
  const file = d.geometry?.fileOrientation ?? 1;
  return !!d.xmpOrientation && d.xmpOrientation !== file;
}

const edgesOf = (d: AdobeDevelop) => {
  const n = reader(d);
  return { left: n('CropLeft', 0), top: n('CropTop', 0), right: n('CropRight', 1), bottom: n('CropBottom', 1) };
};
const cropArea = (d: AdobeDevelop) => {
  const e = edgesOf(d);
  return 1 - (e.right - e.left) * (e.bottom - e.top);
};

/** The full edit for a photo, from its Adobe settings. */
export function adobeToAdjustments(d: AdobeDevelop): Record<string, any> {
  const n = reader(d);
  const v3: V3Controls = defaultV3Controls();

  // White balance.
  const wb = d.values.WhiteBalance ?? 'As Shot';
  if (wb !== 'As Shot') {
    if (d.asShotXy && d.values.Temperature) {
      const target = whiteXy(n('Temperature', 5500), n('Tint') * T.tintDuv);
      Object.assign(v3, whiteBalanceShift(d.asShotXy, target));
    } else if (!d.asShotXy) {
      // JPEG, TIFF: Adobe's temperature and tint are already relative.
      v3.temperature = clamp(n('Temperature') * T.jpegTemperature, -100, 100);
      v3.tint = clamp(n('Tint') * T.jpegTint, -100, 100);
    }
  }

  v3.vibrance = clamp(n('Vibrance') * T.vibrance, -100, 100);
  v3.saturation = clamp(n('Saturation') * T.saturation, -100, 100);
  v3.bands = HUES.map((h) => [
    clamp(n(`HueAdjustment${h}`) * T.hslHueDegrees, -60, 60),
    clamp(n(`SaturationAdjustment${h}`) * T.hslSaturation, -100, 100),
    clamp(n(`LuminanceAdjustment${h}`) * T.hslLuminance, -100, 100),
  ]);

  // Colour grading (Lightroom 10 on), or the split toning before it.
  const wheel = (prefix: string): number[] => [
    clamp(n(`${prefix}Hue`), 0, 360),
    clamp(n(`${prefix}Sat`), 0, 100),
    clamp(n(`${prefix}Lum`), -100, 100),
  ];
  if (['Global', 'Shadow', 'Midtone', 'Highlight'].some((w) => `ColorGrade${w}Hue` in d.values)) {
    v3.grading = [
      wheel('ColorGradeGlobal'),
      wheel('ColorGradeShadow'),
      wheel('ColorGradeMidtone'),
      wheel('ColorGradeHighlight'),
    ];
  } else {
    v3.grading = [
      [0, 0, 0],
      [clamp(n('SplitToningShadowHue'), 0, 360), clamp(n('SplitToningShadowSaturation'), 0, 100), 0],
      [0, 0, 0],
      [clamp(n('SplitToningHighlightHue'), 0, 360), clamp(n('SplitToningHighlightSaturation'), 0, 100), 0],
    ];
  }

  v3.curve = adobeCurveToKnots(d.curves.ToneCurvePV2012) ?? [...IDENTITY];
  v3.channel_curves = ['Red', 'Green', 'Blue'].map(
    (c) => adobeCurveToKnots(d.curves[`ToneCurvePV2012${c}`]) ?? [...IDENTITY],
  );

  v3.detail = {
    ...v3.detail,
    sharpening: clamp(n('Sharpness') * T.sharpening, 0, 100),
    texture: clamp(n('Texture') * T.texture, -100, 100),
    clarity: clamp(n('Clarity2012') * T.clarity, -100, 100),
    dehaze: clamp(n('Dehaze') * T.dehaze, -100, 100),
    luminance_noise: clamp(n('LuminanceSmoothing') * T.luminanceNoise, 0, 100),
    color_noise: clamp(n('ColorNoiseReduction') * T.colorNoise, 0, 100),
  };
  v3.effects = {
    ...v3.effects,
    vignette_amount: clamp(n('PostCropVignetteAmount'), -100, 100),
    vignette_midpoint: clamp(n('PostCropVignetteMidpoint', 50), 0, 100),
    vignette_roundness: clamp(n('PostCropVignetteRoundness'), -100, 100),
    vignette_feather: clamp(n('PostCropVignetteFeather', 50), 0, 100),
    grain_amount: clamp(n('GrainAmount'), 0, 100),
    grain_size: clamp(n('GrainSize', 25), 0, 100),
    grain_roughness: clamp(n('GrainFrequency', 50), 0, 100),
  };
  v3.calibration = {
    shadows_tint: clamp(n('ShadowTint'), -100, 100),
    red_hue: clamp(n('RedHue'), -100, 100),
    red_saturation: clamp(n('RedSaturation'), -100, 100),
    green_hue: clamp(n('GreenHue'), -100, 100),
    green_saturation: clamp(n('GreenSaturation'), -100, 100),
    blue_hue: clamp(n('BlueHue'), -100, 100),
    blue_saturation: clamp(n('BlueSaturation'), -100, 100),
  };

  // Rotation in Lightroom (a different orientation from the file's), then
  // the crop, placed on the photo as it's then shown.
  const fileOrientation = d.geometry?.fileOrientation ?? 1;
  const shown = d.xmpOrientation || fileOrientation;
  const from = ORIENTATION[fileOrientation] ?? ORIENTATION[1];
  const to = ORIENTATION[shown] ?? ORIENTATION[1];
  const orientationSteps = (((to.turns - from.turns) % 4) + 4) % 4;
  const mirrored = from.mirror !== to.mirror;
  const angle = n('CropAngle') * T.angleSign * (to.mirror ? -1 : 1);
  let crop = null;
  if (d.values.HasCrop === 'True' && d.geometry) {
    const swap = (5 <= shown && shown <= 8) !== (5 <= fileOrientation && fileOrientation <= 8);
    const width = swap ? d.geometry.height : d.geometry.width;
    const height = swap ? d.geometry.width : d.geometry.height;
    crop = cropToPixels(cropForOrientation(edgesOf(d), shown), width, height, angle);
  }

  const lens = d.values.LensProfileEnable === '1' ? { lensCorrectionMode: 'auto' } : {};
  return normalizeLoadedAdjustments({
    ...INITIAL_ADJUSTMENTS,
    exposure: clamp(n('Exposure2012') * T.exposure, -5, 5),
    contrast: clamp(n('Contrast2012') * T.contrast, -100, 100),
    highlights: clamp(n('Highlights2012') * T.highlights, -100, 100),
    shadows: clamp(n('Shadows2012') * T.shadows, -100, 100),
    whites: clamp(n('Whites2012') * T.whites, -100, 100),
    blacks: clamp(n('Blacks2012') * T.blacks, -100, 100),
    orientationSteps,
    flipHorizontal: mirrored,
    rotation: Math.abs(angle) > 0.01 ? clamp(angle, -45, 45) : 0,
    crop,
    ...lens,
    v3,
    importedFrom: { app: 'Lightroom', processVersion: d.values.ProcessVersion ?? null, source: d.source },
  } as any);
}
