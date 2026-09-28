/** Development edits adopt v3; shared sliders and geometry are retained. */
export function currentColorEngine<T extends Record<string, any>>(edits: T): T {
  const old = edits.processVersion == null || [0, 1, 2].includes(edits.processVersion);
  // Keep unknown versions intact so the backend rejects rather than misrenders them.
  if (!old && edits.processVersion !== 3) return edits;
  const pipeline = edits.v3Pipeline;
  const migratePin =
    pipeline?.schema === 1 &&
    ['v3-stable-input-1', 'v3-stable-input-2'].includes(pipeline.engine) &&
    ['profiled-display-cube-or-wide-gamut-bypass-1', 'profiled-display-cube-p3-or-compress-1'].includes(
      pipeline.input_policy,
    );
  const result: Record<string, any> = { ...edits, processVersion: 3 };
  if (old) result.toneMapper = 'resolve';
  delete result.v3PreviousVersion;
  delete result.v3PreviousToneMapper;
  if (migratePin) {
    result.v3Pipeline = {
      ...pipeline,
      engine: 'v3-stable-input-2',
      input_policy: 'profiled-display-cube-p3-or-compress-1',
    };
  }
  if (
    old ||
    (migratePin &&
      (pipeline.engine !== result.v3Pipeline.engine || pipeline.input_policy !== result.v3Pipeline.input_policy))
  )
    delete result.v3Input;
  return result as T;
}
