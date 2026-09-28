// Interpretation belongs to a photograph, not to a transferable look.
export const PHOTO_OWNED_ADJUSTMENTS = ['v3Input', 'v3Pipeline', 'v3RawRecovery'] as const;

export function transferableAdjustments<T extends object>(adjustments: T): Partial<T> {
  const result = { ...adjustments };
  // A preset cannot select a retired renderer or restore switch-back metadata.
  delete (result as Record<string, unknown>).processVersion;
  delete (result as Record<string, unknown>).v3PreviousVersion;
  delete (result as Record<string, unknown>).v3PreviousToneMapper;
  for (const key of PHOTO_OWNED_ADJUSTMENTS) delete (result as Record<string, unknown>)[key];
  return result;
}

export function photoOwnedAdjustments(adjustments: object): Record<string, unknown> {
  return Object.fromEntries(
    PHOTO_OWNED_ADJUSTMENTS.filter((key) => Object.prototype.hasOwnProperty.call(adjustments, key)).map((key) => [
      key,
      (adjustments as Record<string, unknown>)[key],
    ]),
  );
}
