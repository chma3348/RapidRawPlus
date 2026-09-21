import assert from 'node:assert/strict';
import { test } from 'node:test';
import { defaultV3Controls, defaultV3Range, evaluateV3Curve, mixV3Controls } from '../src/utils/colorV3.ts';

test('preset intensity preserves versions and grading hue while fading creative amounts', () => {
  const preset = { ...defaultV3Controls(), exposure: 2 };
  preset.bands[0] = [40, 80, 20];
  preset.grading[0] = [240, 80, 20];
  for (const intensity of [0, 25, 50, 100]) {
    const result = mixV3Controls(preset, intensity);
    assert.equal(result.revision, 1);
    assert.equal(result.exposure, (2 * intensity) / 100);
    assert.equal(result.pivot, 0.18);
    assert.equal(result.bands[0][0], (40 * intensity) / 100);
    assert.equal(result.grading[0][0], 240);
    assert.equal(result.grading[0][1], (80 * intensity) / 100);
  }
  assert.equal(preset.bands[0][0], 40, 'mixing mutated the stored preset');
});

test('curves stay monotonic, interpolate knots and extend highlight headroom', () => {
  for (const curve of [
    [0, 0.01, 0.02, 0.03, 1],
    [0, 0.97, 0.98, 0.99, 1],
    [0, 0.15, 0.5, 0.85, 1],
  ]) {
    let previous = 0;
    for (let i = 0; i <= 2000; i++) {
      const value = evaluateV3Curve(curve, i / 1000);
      assert.ok(value >= previous - 1e-12);
      previous = value;
    }
    assert.ok(previous > 1);
    curve.forEach((v, i) => assert.equal(evaluateV3Curve(curve, i / 4), v));
  }
});

test('advanced preset intensity preserves range targets and blends curves toward identity', () => {
  const preset = {
    ...defaultV3Controls(),
    curve: [0, 0.1, 0.4, 0.8, 1],
    ranges: [{ ...defaultV3Range(), adjustment: [30, 50, -20] }],
  };
  for (const intensity of [0, 50, 100]) {
    const mixed = mixV3Controls(preset, intensity);
    assert.deepEqual(mixed.ranges[0].center, preset.ranges[0].center);
    assert.deepEqual(mixed.ranges[0].width, preset.ranges[0].width);
    assert.equal(mixed.ranges[0].adjustment[0], (30 * intensity) / 100);
    assert.equal(mixed.curve[1], 0.25 + ((0.1 - 0.25) * intensity) / 100);
  }
  assert.deepEqual(JSON.parse(JSON.stringify(preset)), preset);
});

test('partial presets fill neutral arrays without sharing mutable defaults', () => {
  const result = mixV3Controls({ exposure: 1 }, 50);
  assert.equal(result.bands.length, 8);
  assert.equal(result.grading.length, 4);
  result.bands[0][0] = 60;
  assert.equal(defaultV3Controls().bands[0][0], 0);
});

test('preset intensity fades detail toward neutral, threshold included', () => {
  const preset = { ...defaultV3Controls(), detail: { ...defaultV3Controls().detail, clarity: 80, threshold: 55 } };
  const half = mixV3Controls(preset, 50);
  assert.equal(half.detail.clarity, 40);
  assert.equal(half.detail.threshold, 35);
  assert.deepEqual(mixV3Controls(preset, 0).detail, defaultV3Controls().detail);
  // Older presets saved before detail existed still load as neutral detail.
  const legacy = { ...defaultV3Controls() };
  delete legacy.detail;
  assert.deepEqual(mixV3Controls(legacy, 100).detail, defaultV3Controls().detail);
});

test('preset intensity fades effect amounts but keeps their shape', () => {
  const base = defaultV3Controls();
  const preset = { ...base, effects: { ...base.effects, vignette_amount: -60, vignette_midpoint: 20, grain_amount: 40, grain_size: 80 } };
  const half = mixV3Controls(preset, 50);
  assert.equal(half.effects.vignette_amount, -30);
  assert.equal(half.effects.grain_amount, 20);
  assert.equal(half.effects.vignette_midpoint, 20);
  assert.equal(half.effects.grain_size, 80);
});

test('preset intensity fades channel curves toward identity', () => {
  const base = defaultV3Controls();
  const preset = { ...base, channel_curves: [[0, 0.45, 0.5, 0.75, 1], base.channel_curves[1], base.channel_curves[2]] };
  const half = mixV3Controls(preset, 50);
  assert.ok(Math.abs(half.channel_curves[0][1] - 0.35) < 1e-9);
  assert.deepEqual(half.channel_curves[1], base.channel_curves[1]);
  const legacy = { ...base };
  delete legacy.channel_curves;
  assert.deepEqual(mixV3Controls(legacy, 100).channel_curves, base.channel_curves);
});
