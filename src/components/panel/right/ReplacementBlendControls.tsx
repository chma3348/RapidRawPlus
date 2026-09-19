import { useEffect, useState } from 'react';
import Slider from '../../ui/Slider';

export type ReplacementBlendOptions = { improved: boolean; transition: number; appearance: number };

/** Draft controls are explicit: changing a slider never triggers generation. */
export default function ReplacementBlendControls({
  patch,
  disabled,
  onApply,
}: {
  patch: any;
  disabled: boolean;
  onApply: (id: string, options: ReplacementBlendOptions) => Promise<boolean>;
}) {
  const saved = patch.patchData?.replacementBlend;
  const run = patch.patchData?.replacementRunId ?? patch.patchData?.reconstructDebugRunId;
  const [transition, setTransition] = useState(saved?.options?.transition ?? 40);
  const [appearance, setAppearance] = useState(saved?.options?.appearance ?? 35);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setTransition(saved?.options?.transition ?? 40);
    setAppearance(saved?.options?.appearance ?? 35);
  }, [patch.id, run, saved?.options?.transition, saved?.options?.appearance]);
  const apply = async (improved: boolean) => {
    setBusy(true);
    try {
      await onApply(patch.id, { improved, transition, appearance });
    } finally {
      setBusy(false);
    }
  };
  const inactive = disabled || busy;
  const buttonClass =
    'w-full px-3 py-2 rounded-md bg-bg-primary text-text-primary text-sm hover:bg-card-active focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-50 disabled:cursor-not-allowed';
  return (
    <div className="space-y-3 pt-3 border-t border-surface">
      <div className="text-sm font-medium text-text-primary">Blend this result</div>
      <p className="text-xs text-text-primary leading-relaxed">
        Uses the same saved clouds or content. No new AI generation.
      </p>
      <fieldset disabled={inactive} className="space-y-2 min-w-0">
        <Slider
          label="Transition width"
          min={0}
          max={160}
          step={5}
          defaultValue={40}
          value={transition}
          onChange={(e: any) => setTransition(Number(e.target.value))}
        />
        <Slider
          label="Match appearance"
          min={0}
          max={100}
          step={5}
          defaultValue={35}
          value={appearance}
          suffix="%"
          onChange={(e: any) => setAppearance(Number(e.target.value))}
        />
      </fieldset>
      <p className="text-xs text-text-primary leading-relaxed">
        Transition can extend into adjacent recognized sky. Foreground edges and red markings are guarded. Matching
        uses healthy surrounding sky to adjust brightness and saturation; 0% keeps the generated tones.
      </p>
      <button type="button" className={buttonClass} disabled={inactive} onClick={() => apply(true)}>
        {busy ? 'Blending saved result…' : 'Apply improved blend'}
      </button>
      <button
        type="button"
        className={buttonClass}
        disabled={inactive || !saved?.options?.improved}
        onClick={() => apply(false)}
      >
        Restore original blend
      </button>
      <p role="status" aria-live="polite" className="text-xs text-text-primary leading-relaxed">
        Showing {saved?.options?.improved ? 'improved' : 'original'} blend. Slider changes apply with the button above.
        {saved?.options?.improved &&
          !saved.canExpand &&
          ' Transition stays inside the selection: no safe sky expansion is available, or a subtractive refinement protects the area.'}
      </p>
    </div>
  );
}
