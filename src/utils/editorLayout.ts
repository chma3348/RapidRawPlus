/**
 * How a person has arranged the editor: Basic or Advanced adjustments,
 * which Advanced sections are open, their order and which are hidden, which
 * side the panel docks on, and whether the panel rail shows labels.
 *
 * Saved in app settings (`editorLayout`); anything missing falls back to
 * the defaults here, and sections added in later versions appear in their
 * default place, so an old saved layout never hides new controls.
 */
export type AdjustmentsMode = 'basic' | 'advanced';

export interface EditorLayout {
  adjustmentsMode: AdjustmentsMode;
  /** Advanced section id → open. */
  openSections: Record<string, boolean>;
  /** Advanced section ids, in the order shown. */
  sectionOrder: string[];
  /** Advanced section ids moved to "Show sections". */
  hiddenSections: string[];
  panelSide: 'right' | 'left';
  /** Show text labels beside the panel rail's icons. */
  showRailLabels: boolean;
}

/**
 * The Advanced sections, in their default order, and whether each starts
 * open. The order follows the editing workflow Lightroom and darktable
 * share: set the starting look and fix white balance, then exposure, then
 * presence (local contrast and colour intensity), then shape tone and
 * colour, then clean up (detail, lens), then creative finishing, and the
 * set-once camera settings last.
 */
export const ADVANCED_SECTIONS: Array<{ id: string; defaultOpen: boolean }> = [
  { id: 'look', defaultOpen: false },
  { id: 'whiteBalance', defaultOpen: true },
  { id: 'light', defaultOpen: true },
  { id: 'presence', defaultOpen: true },
  { id: 'toneCurve', defaultOpen: false },
  { id: 'colorMixer', defaultOpen: false },
  { id: 'colorGrading', defaultOpen: false },
  { id: 'detail', defaultOpen: false },
  { id: 'optics', defaultOpen: false },
  { id: 'effects', defaultOpen: false },
  { id: 'calibration', defaultOpen: false },
  { id: 'raw', defaultOpen: false },
];

/** The first release's default order. A saved layout still in exactly this
 *  order was never rearranged, so it moves to the current default. */
const FIRST_DEFAULT_ORDER = [
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

export function defaultEditorLayout(): EditorLayout {
  return {
    adjustmentsMode: 'basic',
    openSections: Object.fromEntries(ADVANCED_SECTIONS.map((s) => [s.id, s.defaultOpen])),
    sectionOrder: ADVANCED_SECTIONS.map((s) => s.id),
    hiddenSections: [],
    panelSide: 'right',
    showRailLabels: false,
  };
}

const known = new Set(ADVANCED_SECTIONS.map((s) => s.id));

/** A saved layout made whole: unknown ids dropped, new sections added. */
export function resolveEditorLayout(saved?: Partial<EditorLayout> | null): EditorLayout {
  const base = defaultEditorLayout();
  if (!saved || typeof saved !== 'object') return base;

  const untouched =
    Array.isArray(saved.sectionOrder) &&
    saved.sectionOrder.length === FIRST_DEFAULT_ORDER.length &&
    saved.sectionOrder.every((id, i) => id === FIRST_DEFAULT_ORDER[i]);
  // "Color" became "Presence" (it gained texture, clarity and dehaze).
  const renamed = (id: string) => (id === 'color' ? 'presence' : id);
  const savedOrder =
    Array.isArray(saved.sectionOrder) && !untouched
      ? saved.sectionOrder.map(renamed).filter((id) => known.has(id))
      : [];
  const order = [...new Set(savedOrder)];
  // A section the saved order does not know goes after the section that
  // precedes it by default, so it lands near its neighbours.
  ADVANCED_SECTIONS.forEach((section, i) => {
    if (order.includes(section.id)) return;
    const before = ADVANCED_SECTIONS.slice(0, i)
      .map((s) => s.id)
      .reverse()
      .find((id) => order.includes(id));
    order.splice(before ? order.indexOf(before) + 1 : 0, 0, section.id);
  });

  return {
    adjustmentsMode: saved.adjustmentsMode === 'advanced' ? 'advanced' : 'basic',
    openSections: {
      ...base.openSections,
      ...Object.fromEntries(Object.entries(saved.openSections ?? {}).map(([id, open]) => [renamed(id), open])),
    },
    sectionOrder: order,
    hiddenSections: Array.isArray(saved.hiddenSections)
      ? saved.hiddenSections.map(renamed).filter((id) => known.has(id))
      : [],
    panelSide: saved.panelSide === 'left' ? 'left' : 'right',
    showRailLabels: saved.showRailLabels === true,
  };
}
