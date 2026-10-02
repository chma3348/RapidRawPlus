import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  AdobeDevelop,
  adobeCurveToKnots,
  adobeToAdjustments,
  cropForOrientation,
  cropToPixels,
  hasAdobeEdits,
  planckXy,
  whiteBalanceShift,
  whiteXy,
} from '../src/utils/adobeImport';

const develop = (values: Record<string, string>, extra: Partial<AdobeDevelop> = {}): AdobeDevelop => ({
  values,
  curves: {},
  localCorrections: 0,
  retouchSpots: 0,
  source: 'sidecar',
  geometry: { width: 6000, height: 4000, fileOrientation: 1 },
  ...extra,
});
const near = (a: number, b: number, tol: number, what: string) =>
  assert.ok(Math.abs(a - b) <= tol, `${what}: ${a} is not within ${tol} of ${b}`);

test('a straight Adobe curve is no curve; a lifted midtone lifts the middle knot', () => {
  assert.equal(
    adobeCurveToKnots([
      [0, 0],
      [255, 255],
    ]),
    null,
  );
  const lifted = adobeCurveToKnots([
    [0, 0],
    [128, 160],
    [255, 255],
  ])!;
  assert.equal(lifted[0], 0);
  assert.equal(lifted[4], 1);
  assert.ok(lifted[2] > 0.5, `middle knot ${lifted[2]}`);
  for (let i = 1; i < 5; i++) assert.ok(lifted[i] - lifted[i - 1] >= 0.0099);
});

test('daylight sits where it should and tint moves off the curve', () => {
  const [x, y] = planckXy(6504);
  near(x, 0.3135, 0.003, 'x at 6504 K');
  near(y, 0.3237, 0.003, 'y at 6504 K');
  const green = whiteXy(5500, 0.01);
  assert.ok(green[1] > planckXy(5500)[1], 'positive Duv is greener (higher y)');
});

test('choosing a warmer white than as shot warms the photo; the same white does nothing', () => {
  const asShot = planckXy(4500);
  const same = whiteBalanceShift(asShot, planckXy(4500));
  near(same.temperature, 0, 0.01, 'temperature');
  near(same.tint, 0, 0.01, 'tint');
  // Telling Lightroom the light was bluer (higher kelvin) makes the picture warmer.
  const warmer = whiteBalanceShift(asShot, planckXy(6500));
  assert.ok(warmer.temperature > 10, `temperature ${warmer.temperature}`);
  // The engine's sliders, at those values, turn exactly the chosen white neutral:
  // gains of 2^(±0.006·T + 0.006·t) on the long and short cone channels.
  const B = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
  ];
  const lms = ([x, y]: [number, number]) => B.map((r) => r[0] * (x / y) + r[1] + r[2] * ((1 - x - y) / y));
  const [a, t] = [lms(asShot), lms(planckXy(6500))];
  near(Math.log2(a[0] / t[0] / (a[1] / t[1])), 0.006 * warmer.temperature + 0.006 * warmer.tint, 1e-9, 'long');
  near(Math.log2(a[2] / t[2] / (a[1] / t[1])), -0.006 * warmer.temperature + 0.006 * warmer.tint, 1e-9, 'short');
});

test('crop edges follow the photo upright', () => {
  // The real Lightroom print layout: a portrait stored sideways (orientation 8)
  // padded top and bottom as stored, which is left and right as shown.
  const shown = cropForOrientation({ left: 0, top: -0.264, right: 1, bottom: 1.236 }, 8);
  assert.deepEqual(shown, { left: -0.264, right: 1.236, top: 0, bottom: 1 });
  // A crop reaching outside the photo can't be shown here.
  assert.equal(cropToPixels(shown, 4000, 6000, 0), null);
  // Orientation 6 turns the stored top into the right as shown.
  assert.deepEqual(cropForOrientation({ left: 0.1, top: 0.2, right: 0.9, bottom: 0.7 }, 6), {
    left: 0.30000000000000004,
    right: 0.8,
    top: 0.1,
    bottom: 0.9,
  });
});

test('a centred crop stays centred whatever the angle', () => {
  const c = cropToPixels({ left: 0.1, top: 0.1, right: 0.9, bottom: 0.9 }, 6000, 4000, 5)!;
  assert.deepEqual(c, { unit: 'px', x: 600, y: 400, width: 4800, height: 3200 });
});

test('default settings are not edits; any develop change is', () => {
  const defaults = develop({
    ProcessVersion: '15.4',
    WhiteBalance: 'As Shot',
    Temperature: '5200',
    Exposure2012: '0.00',
    Sharpness: '40',
    ColorNoiseReduction: '25',
    LensProfileEnable: '1',
  });
  assert.equal(hasAdobeEdits(defaults), false);
  assert.equal(hasAdobeEdits(develop({ ...defaults.values, Exposure2012: '+0.35' })), true);
  assert.equal(hasAdobeEdits(develop({ ...defaults.values, WhiteBalance: 'Custom' })), true);
  assert.equal(hasAdobeEdits(develop(defaults.values, { xmpOrientation: 6 })), true);
});

test('a full Lightroom edit comes across in the engine’s terms', () => {
  const adj = adobeToAdjustments(
    develop(
      {
        ProcessVersion: '15.4',
        WhiteBalance: 'Custom',
        Temperature: '6500',
        Tint: '+0',
        Exposure2012: '+1.00',
        Highlights2012: '-40',
        Clarity2012: '+20',
        Vibrance: '+15',
        HueAdjustmentBlue: '-50',
        SaturationAdjustmentOrange: '-20',
        ColorGradeShadowHue: '210',
        ColorGradeShadowSat: '18',
        ColorGradeHighlightHue: '45',
        ColorGradeHighlightSat: '12',
        PostCropVignetteAmount: '-25',
        LensProfileEnable: '1',
        HasCrop: 'True',
        CropLeft: '0.1',
        CropTop: '0.1',
        CropRight: '0.9',
        CropBottom: '0.9',
        CropAngle: '0',
      },
      { asShotXy: planckXy(4500) },
    ),
  );
  assert.equal(adj.exposure, 1);
  assert.equal(adj.highlights, -40);
  assert.equal(adj.processVersion, 3);
  assert.equal(adj.lensCorrectionMode, 'auto');
  assert.deepEqual(adj.crop, { unit: 'px', x: 600, y: 400, width: 4800, height: 3200 });
  assert.ok(adj.v3.temperature > 10);
  assert.equal(adj.v3.vibrance, 15);
  assert.deepEqual(adj.v3.bands[5], [-15, 0, 0]);
  assert.deepEqual(adj.v3.bands[1], [0, -20, 0]);
  assert.deepEqual(adj.v3.grading[1], [210, 18, 0]);
  assert.deepEqual(adj.v3.grading[3], [45, 12, 0]);
  assert.equal(adj.v3.detail.clarity, 20);
  assert.equal(adj.v3.effects.vignette_amount, -25);
  assert.equal(adj.importedFrom.app, 'Lightroom');
});

test('a photo turned in Lightroom turns here too', () => {
  const adj = adobeToAdjustments(develop({ Exposure2012: '0' }, { xmpOrientation: 6 }));
  assert.equal(adj.orientationSteps, 1);
  assert.equal(adj.flipHorizontal, false);
});
