import { useTranslation } from 'react-i18next';
import Switch from '../../ui/Switch';
import Text from '../../ui/Text';
import { TextVariants } from '../../../types/typography';
import { ColorSample, ColorSelectParameters } from './Masks';

const SWATCHES = [
  { hue: 358, width: 35, c: '#f87171' },
  { hue: 25, width: 45, c: '#fb923c' },
  { hue: 60, width: 40, c: '#facc15' },
  { hue: 115, width: 90, c: '#4ade80' },
  { hue: 180, width: 60, c: '#2dd4bf' },
  { hue: 225, width: 60, c: '#60a5fa' },
  { hue: 280, width: 55, c: '#a78bfa' },
  { hue: 330, width: 50, c: '#f472b6' },
];

/**
 * The colour select's controls, shared by Masks and Retouch: the Contiguous
 * switch (Photoshop's Magic Wand when on, its Color Range when off), the
 * sample chips, and optionally the colour-band swatches.
 */
export default function ColorSampleControls({
  parameters,
  onChange,
  showSwatches = false,
}: {
  parameters: ColorSelectParameters;
  onChange: (changes: Partial<ColorSelectParameters>) => void;
  showSwatches?: boolean;
}) {
  const { t } = useTranslation();
  const contiguous = !!parameters?.contiguous;
  const samples: Array<ColorSample> = Array.isArray(parameters?.samples) ? parameters.samples : [];
  return (
    <div className="space-y-2">
      <Switch
        checked={contiguous}
        label={t('editor.masks.colorSelect.contiguous', 'Contiguous')}
        tooltip={t(
          'editor.masks.colorSelect.contiguousTooltip',
          'On: only the area connected to where you click (Magic Wand). Off: that colour anywhere in the photo.',
        )}
        onChange={(value) =>
          // A colour band has no point to grow from, so the wand starts from
          // the next click instead (not from a click made before the band).
          onChange(
            value
              ? {
                  contiguous: true,
                  swatchHue: undefined,
                  swatchWidth: undefined,
                  ...(parameters?.swatchHue !== undefined && samples.length === 0
                    ? { targetX: -10000, targetY: -10000 }
                    : {}),
                }
              : { contiguous: false },
          )
        }
      />
      <Text variant={TextVariants.small} className="opacity-70">
        {contiguous
          ? t(
              'editor.masks.colorSelect.hintContiguous',
              'Click the photo to select the connected area of that colour; Shift-click adds an area, Option-click removes one.',
            )
          : t('editor.masks.colorSelect.hint')}
      </Text>
      {samples.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {samples.map((sample, i) => (
            <button
              key={i}
              type="button"
              onClick={() => onChange({ samples: samples.filter((_, j) => j !== i) })}
              className={`px-2 py-0.5 rounded text-xs transition-colors hover:bg-red-500/40 ${
                sample.subtract ? 'bg-bg-tertiary text-text-secondary' : 'bg-card-active text-text-primary'
              }`}
              data-tooltip={t('editor.masks.colorSelect.removeSample')}
            >
              {sample.subtract ? '− ' : ''}
              {t('editor.masks.colorSelect.sample')} {i + 1} ×
            </button>
          ))}
        </div>
      )}
      {showSwatches && !contiguous && (
        <div className="flex items-center gap-1">
          {SWATCHES.map((sw) => (
            <button
              key={sw.hue}
              type="button"
              onClick={() => onChange({ swatchHue: sw.hue, swatchWidth: sw.width, samples: [] })}
              className={`w-5 h-5 rounded-full border-2 transition-transform hover:scale-110 ${
                parameters?.swatchHue === sw.hue ? 'border-white' : 'border-transparent'
              }`}
              style={{ backgroundColor: sw.c }}
              data-tooltip={t('editor.masks.colorSelect.swatchTooltip')}
            />
          ))}
        </div>
      )}
    </div>
  );
}
