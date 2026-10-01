import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ADVANCED_SECTIONS, defaultEditorLayout, resolveEditorLayout } from '../src/utils/editorLayout';

const ids = ADVANCED_SECTIONS.map((s) => s.id);

test('nothing saved means the defaults: Basic mode, every section in order', () => {
  const layout = resolveEditorLayout(undefined);
  assert.equal(layout.adjustmentsMode, 'basic');
  assert.deepEqual(layout.sectionOrder, ids);
  assert.deepEqual(layout.hiddenSections, []);
  assert.equal(layout.panelSide, 'right');
  assert.equal(layout.openSections.light, true);
  assert.equal(layout.openSections.calibration, false);
});

test('a saved order is kept, and sections it does not know appear near their neighbours', () => {
  // A rearranged older layout (Look moved to the top) that knew neither
  // "colorMixer" nor "raw", with the old "color" section.
  const saved = {
    adjustmentsMode: 'advanced' as const,
    sectionOrder: [
      'look',
      'light',
      'whiteBalance',
      'toneCurve',
      'color',
      'colorGrading',
      'detail',
      'effects',
      'optics',
      'calibration',
      'gone',
    ],
    hiddenSections: ['calibration', 'gone'],
    openSections: { look: true, color: false },
  };
  const layout = resolveEditorLayout(saved);
  assert.equal(layout.adjustmentsMode, 'advanced');
  assert.deepEqual(layout.sectionOrder, [
    'look',
    'light',
    'whiteBalance',
    'toneCurve',
    'colorMixer',
    'presence',
    'colorGrading',
    'detail',
    'effects',
    'optics',
    'calibration',
    'raw',
  ]);
  assert.deepEqual(layout.hiddenSections, ['calibration']);
  assert.equal(layout.openSections.look, true);
  assert.equal(layout.openSections.presence, false, '"color" carries over as "presence"');
  assert.equal(layout.openSections.light, true, 'unsaved open states fall back to defaults');
  assert.equal(new Set(layout.sectionOrder).size, ids.length);
});

test('a layout still in the first default order moves to the workflow order', () => {
  const first = [
    'light',
    'whiteBalance',
    'toneCurve',
    'color',
    'colorMixer',
    'colorGrading',
    'detail',
    'effects',
    'optics',
    'calibration',
    'look',
    'raw',
  ];
  const layout = resolveEditorLayout({ sectionOrder: first, hiddenSections: ['color'] });
  assert.deepEqual(layout.sectionOrder, ids);
  assert.deepEqual(ids.slice(0, 4), ['look', 'whiteBalance', 'light', 'presence']);
  assert.deepEqual(layout.hiddenSections, ['presence']);
});

test('nonsense values fall back safely', () => {
  const layout = resolveEditorLayout({
    adjustmentsMode: 'expert' as any,
    panelSide: 'top' as any,
    sectionOrder: 'x' as any,
  });
  assert.deepEqual(layout, { ...defaultEditorLayout() });
});
