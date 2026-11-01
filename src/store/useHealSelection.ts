import { create } from 'zustand';

/**
 * The heal or clone spot selected on the photo, shared by the canvas (which
 * draws it and its source) and the Retouch panel (Refresh source, Delete).
 * A spot is one brush stroke: line `lineIndex` of sub-mask `subMaskId` in
 * patch `patchId`.
 */
export interface HealSpotRef {
  key: string;
  patchId: string;
  subMaskId: string;
  lineIndex: number;
}

export type HealAction = 'refresh' | 'delete';

export interface Offset {
  x: number;
  y: number;
}

/** A heal or clone stroke as stored on its brush sub-mask. */
export interface HealLine {
  points?: Offset[];
  brushSize?: number;
  /** Where the user dragged the source to, relative to the stroke. */
  cloneOffset?: Offset;
  /** Where the heal chose to borrow from, reported back after each run. */
  autoOffset?: Offset;
  /** Sources turned down with Refresh source, kept clear of next time. */
  rejectedSources?: Offset[];
}

/** The lines of a brush sub-mask's parameters, if it has any. */
export const healLines = (parameters: unknown): HealLine[] | undefined => {
  const lines = (parameters as { lines?: unknown } | null | undefined)?.lines;
  return Array.isArray(lines) ? (lines as HealLine[]) : undefined;
};

interface HealSelectionState {
  selected: HealSpotRef | null;
  select: (spot: HealSpotRef | null) => void;
  /** Asked for by the panel or a key; the canvas, which owns the spots, acts. */
  action: { kind: HealAction; id: number } | null;
  request: (kind: HealAction) => void;
}

export const useHealSelection = create<HealSelectionState>((set) => ({
  selected: null,
  select: (selected) => set({ selected }),
  action: null,
  request: (kind) => set((s) => ({ action: { kind, id: (s.action?.id ?? 0) + 1 } })),
}));
