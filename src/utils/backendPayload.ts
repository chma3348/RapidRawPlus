import type { Adjustments, AiPatch, MaskContainer } from './adjustments';

/** Sub-mask parameters that can hold a bitmap (megabytes of base64). */
const MASK_DATA_KEYS = ['mask_data_base64', 'maskDataBase64'] as const;

/**
 * The fill and mask bitmaps the backend holds, by patch or sub-mask id, with
 * the very data it was sent. A render leaves out (sends null for) data the
 * backend already has, and it fills the gap from its own copy
 * (adjustment_utils.rs hydrate_*). Only data it has: when the data under an
 * id changes (undo, redo, a variant picked, a fill's mask redrawn), the new
 * data goes in full, or the backend would keep showing the old.
 *
 * `clear()` starts a new generation (the backend forgets what it was sent
 * when it opens a photo): a render still in flight from before records
 * nothing when it returns.
 */
export class SentPatches {
  private sent = new Map<string, unknown>();
  private gen = 0;

  get generation() {
    return this.gen;
  }

  clear() {
    this.sent.clear();
    this.gen += 1;
  }

  delete(id: string) {
    return this.sent.delete(id);
  }

  /** Whether the backend holds exactly `data` under `id`. */
  holds(id: string, data: unknown) {
    return this.sent.has(id) && this.sent.get(id) === data;
  }

  /** What a render sent reached the backend; ignored if the photo has been reopened since it was sent. */
  record(sent: Array<[string, unknown]>, generation: number) {
    if (generation !== this.gen) return;
    for (const [id, data] of sent) this.sent.set(id, data);
  }
}

/**
 * `subMasks` with the bitmaps the backend holds left out; those it lacks
 * are added to `fresh`. The same array back when nothing is left out, and
 * the input is never changed.
 */
export function blankSentSubMasks<T>(subMasks: T[], sent: SentPatches, fresh?: Array<[string, unknown]>): T[] {
  if (!Array.isArray(subMasks)) return subMasks;
  let out: T[] | null = null;
  subMasks.forEach((item, i) => {
    const sm = item as { id?: string; parameters?: Record<string, unknown> | null } | null;
    if (!sm?.id || !sm.parameters) return;
    const id = sm.id;
    const params = sm.parameters;
    const keys = MASK_DATA_KEYS.filter((key) => params[key] !== undefined && params[key] !== null);
    if (keys.length === 0) return;
    // All or nothing: the backend fills every null key from one copy per id.
    if (!keys.every((key) => sent.holds(id, params[key]))) {
      keys.forEach((key) => fresh?.push([id, params[key]]));
      return;
    }
    const parameters = { ...params };
    keys.forEach((key) => (parameters[key] = null));
    out ??= subMasks.slice();
    out[i] = { ...sm, parameters } as T;
  });
  return out ?? subMasks;
}

/**
 * The adjustments to send a render: fill and mask data the backend holds
 * left out, the rest shared with `adjustments` (copied only where something
 * is left out). `fresh` lists the data sent in full, to record once the
 * backend has it.
 */
export function blankSentData(
  adjustments: Adjustments,
  sent: SentPatches,
): { payload: Adjustments; fresh: Array<[string, unknown]> } {
  const fresh: Array<[string, unknown]> = [];
  let payload = adjustments;

  if (Array.isArray(adjustments.aiPatches)) {
    let changed = false;
    const aiPatches = adjustments.aiPatches.map((p: AiPatch) => {
      let next = p;
      // A patch still generating is always sent as it is.
      if (p?.id && p.patchData && !p.isLoading) {
        if (sent.holds(p.id, p.patchData)) next = { ...next, patchData: null };
        else fresh.push([p.id, p.patchData]);
      }
      if (p?.subMasks) {
        const subMasks = blankSentSubMasks(p.subMasks, sent, fresh);
        if (subMasks !== p.subMasks) next = { ...next, subMasks };
      }
      if (next !== p) changed = true;
      return next;
    });
    if (changed) payload = { ...payload, aiPatches };
  }

  if (Array.isArray(adjustments.masks)) {
    let changed = false;
    const masks = adjustments.masks.map((m: MaskContainer) => {
      if (!m?.subMasks) return m;
      const subMasks = blankSentSubMasks(m.subMasks, sent, fresh);
      if (subMasks === m.subMasks) return m;
      changed = true;
      return { ...m, subMasks };
    });
    if (changed) payload = { ...payload, masks };
  }

  return { payload, fresh };
}

/** The geometry the AI tools send (useAiMasking getTransformAdjustments). */
const WARP_KEYS = [
  'transformDistortion',
  'transformVertical',
  'transformHorizontal',
  'transformRotate',
  'transformAspect',
  'transformScale',
  'transformXOffset',
  'transformYOffset',
  'lensDistortionAmount',
  'lensVignetteAmount',
  'lensTcaAmount',
  'lensDistortionParams',
  'lensMaker',
  'lensModel',
  'lensDistortionEnabled',
  'lensTcaEnabled',
  'lensVignetteEnabled',
] as const;

/**
 * What the mask overlay reads from the adjustments: only the warp of the
 * picture colour, luminance and clipped masks sample (generate_mask_overlay
 * → get_cached_full_warped_image). The flat-field correction is part of that
 * warp, so it goes too when there is one; without one this is exactly what
 * the AI tools send, and the backend's one cached warped picture serves both.
 */
export function maskOverlayAdjustments(adjustments: Adjustments): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of WARP_KEYS) out[key] = (adjustments as unknown as Record<string, unknown>)[key];
  if (adjustments.flatFieldProfile) {
    out.flatFieldProfile = adjustments.flatFieldProfile;
    out.flatFieldStrength = adjustments.flatFieldStrength;
  }
  return out;
}

/** Leaves out what JSON leaves out of an object (and writes as null in an array). */
const absentInJson = (v: unknown) => v === undefined || typeof v === 'function' || typeof v === 'symbol';

/**
 * Whether `a` and `b` would write the same JSON, key order aside, without
 * writing it: values shared by reference are skipped at once, and strings
 * (fill data runs to megabytes) are compared, not escaped and copied.
 */
export function sameJson(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a === 'number' && !Number.isFinite(a)) a = null;
  if (typeof b === 'number' && !Number.isFinite(b)) b = null;
  if (absentInJson(a) || absentInJson(b)) return absentInJson(a) && absentInJson(b);
  if (a === b) return true;
  if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return false;
  if (
    typeof (a as { toJSON?: unknown }).toJSON === 'function' ||
    typeof (b as { toJSON?: unknown }).toJSON === 'function'
  ) {
    return JSON.stringify(a) === JSON.stringify(b);
  }
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) {
    const other = b as unknown[];
    if (a.length !== other.length) return false;
    for (let i = 0; i < a.length; i++) {
      // In an array, a missing value is written as null.
      const x = absentInJson(a[i]) ? null : a[i];
      const y = absentInJson(other[i]) ? null : other[i];
      if (!sameJson(x, y)) return false;
    }
    return true;
  }
  const x = a as Record<string, unknown>;
  const y = b as Record<string, unknown>;
  for (const key of Object.keys(x)) if (!sameJson(x[key], y[key])) return false;
  for (const key of Object.keys(y)) {
    if (!Object.prototype.hasOwnProperty.call(x, key) && !absentInJson(y[key])) return false;
  }
  return true;
}
