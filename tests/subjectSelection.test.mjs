import { test } from 'node:test';
import assert from 'node:assert/strict';
import { subjectPrompts } from '../src/utils/subjectSelection.ts';

test('include and exclude clicks accumulate in one selection', () => {
  const start = { x: 20, y: 30 };
  const initial = subjectPrompts({}, start, start, false);
  const correction = { x: 5, y: 6 };
  const result = subjectPrompts({ subjectPoints: initial }, correction, correction, true);
  assert.deepEqual(result, [{ ...start, label: 1 }, { ...correction, label: 0 }]);
  assert.equal(initial.length, 1);
});

test('a new box resets old prompts and normalizes drag direction', () => {
  const result = subjectPrompts({ subjectPoints: [{ x: 999, y: 999, label: 1 }] },
    { x: 80, y: 90 }, { x: 20, y: 30 }, false);
  assert.deepEqual(result, [{ x: 20, y: 30, label: 2 }, { x: 80, y: 90, label: 3 }]);
});

test('legacy selections retain their original prompt; reset does not revive it', () => {
  const legacy = { maskDataBase64: 'mask', startX: 10, startY: 20, endX: 10, endY: 20 };
  const point = { x: 50, y: 60 };
  const result = subjectPrompts(legacy, point, point, false);
  assert.equal(result.length, 2);
  assert.deepEqual(result[0], { x: 10, y: 20, label: 1 });
  assert.equal(subjectPrompts({ ...legacy, subjectPoints: [] }, point, point, false).length, 1);
});

test('negative first clicks and prompt overflow are rejected', () => {
  const point = { x: 10, y: 20 };
  assert.throws(() => subjectPrompts({}, point, point, true), /Include part/);
  assert.throws(() => subjectPrompts({ subjectPoints: Array(256).fill({ ...point, label: 1 }) },
    point, point, false), /256/);
});
