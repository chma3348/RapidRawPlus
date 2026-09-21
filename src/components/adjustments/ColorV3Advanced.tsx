import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import ColorV3Sampler from './ColorV3Sampler';
import { defaultV3Range, evaluateV3Curve, type V3Controls } from '../../utils/colorV3';

export default function ColorV3Advanced({
  values,
  update,
  onDragStateChange,
  inspection,
}: {
  values: V3Controls;
  update: (key: keyof V3Controls, value: unknown) => void;
  onDragStateChange?: (dragging: boolean) => void;
  inspection?: { path: string; edits: unknown };
}) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState(0);
  const index = Math.min(selected, Math.max(0, values.ranges.length - 1));
  const range = values.ranges[index];
  const button =
    'rounded-md px-2 py-1 bg-surface hover:bg-card-active focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-50';
  const curvePath = Array.from(
    { length: 101 },
    (_, i) => `${i === 0 ? 'M' : 'L'} ${i * 2} ${200 - evaluateV3Curve(values.curve, i / 100) * 200}`,
  ).join(' ');
  const changeRange = (group: 'center' | 'width' | 'adjustment', column: number, value: number) =>
    update(
      'ranges',
      values.ranges.map((r, i) =>
        i === index ? { ...r, [group]: r[group].map((v, j) => (j === column ? value : v)) } : r,
      ),
    );
  const rangeSlider = (
    group: 'center' | 'width' | 'adjustment',
    column: number,
    label: string,
    min: number,
    max: number,
    step: number,
  ) =>
    range && (
      <Slider
        key={`${index}-${group}-${column}`}
        label={label}
        value={range[group][column]}
        min={min}
        max={max}
        step={step}
        defaultValue={defaultV3Range()[group][column]}
        onDragStateChange={onDragStateChange}
        onChange={(e: any) => changeRange(group, column, Number(e.target.value))}
      />
    );
  return (
    <>
      <details className="mt-3 text-text-primary" open>
        <summary className="cursor-pointer text-sm font-medium focus-visible:outline-2 focus-visible:outline-accent">
          {t('colorV3.curveTitle', { defaultValue: 'Tone curve' })}
        </summary>
        <p className="my-2 text-sm">
          {t('colorV3.curveHelp', {
            defaultValue:
              'Adjust brightness without reversing tones. The curve continues above white to preserve highlight headroom.',
          })}
        </p>
        <svg
          viewBox="0 0 200 200"
          preserveAspectRatio="none"
          className="w-full max-h-40 bg-surface rounded-md"
          role="img"
          aria-label={t('colorV3.curveGraph', {
            defaultValue: 'Tone curve: input brightness horizontally, output vertically',
          })}
        >
          <path d="M0 200L200 0" fill="none" stroke="currentColor" strokeOpacity="0.3" />
          <path d={curvePath} fill="none" stroke="currentColor" strokeWidth="2" />
        </svg>
        {[1, 2, 3].map((i) => (
          <Slider
            key={i}
            label={t(`colorV3.curvePoint${i}`, { defaultValue: ['', 'Lower curve', 'Middle curve', 'Upper curve'][i] })}
            value={values.curve[i] * 100}
            min={Math.round((values.curve[i - 1] + 0.01) * 100)}
            max={Math.round((values.curve[i + 1] - 0.01) * 100)}
            step={1}
            defaultValue={Math.min(values.curve[i + 1] - 0.01, Math.max(values.curve[i - 1] + 0.01, i / 4)) * 100}
            onDragStateChange={onDragStateChange}
            onChange={(e: any) =>
              update(
                'curve',
                values.curve.map((v, j) => (j === i ? Number(e.target.value) / 100 : v)),
              )
            }
          />
        ))}
        <button type="button" className={button} onClick={() => update('curve', [0, 0.25, 0.5, 0.75, 1])}>
          {t('colorV3.resetCurve', { defaultValue: 'Reset curve' })}
        </button>
      </details>
      <details className="mt-3 text-text-primary">
        <summary className="cursor-pointer text-sm font-medium focus-visible:outline-2 focus-visible:outline-accent">
          {t('colorV3.rangeTitle', { defaultValue: 'Custom color ranges' })}
        </summary>
        <p className="my-2 text-sm">
          {t('colorV3.rangeHelp', {
            defaultValue:
              'Target a hue, chroma and lightness range with soft transitions. Centers use Oklab values before selective color adjustments.',
          })}
        </p>
        <div className="flex gap-2 flex-wrap mb-2">
          <button
            type="button"
            className={button}
            disabled={values.ranges.length >= 8}
            onClick={() => {
              setSelected(values.ranges.length);
              update('ranges', [...values.ranges, defaultV3Range()]);
            }}
          >
            {t('colorV3.addRange', { defaultValue: 'Add range' })}
          </button>
          {range && (
            <button
              type="button"
              className={button}
              onClick={() =>
                update(
                  'ranges',
                  values.ranges.filter((_, i) => i !== index),
                )
              }
            >
              {t('colorV3.removeRange', { defaultValue: 'Remove selected range' })}
            </button>
          )}
        </div>
        {range && (
          <>
            {inspection && (
              <ColorV3Sampler
                {...inspection}
                index={index}
                onCenter={(center) =>
                  update(
                    'ranges',
                    values.ranges.map((r, i) => (i === index ? { ...r, center } : r)),
                  )
                }
              />
            )}
            <label className="text-sm">
              {t('colorV3.selectedRange', { defaultValue: 'Selected range' })}
              <select
                className="my-2 w-full bg-surface rounded-md p-2 focus-visible:outline-2 focus-visible:outline-accent"
                value={index}
                onChange={(e) => setSelected(Number(e.target.value))}
              >
                {values.ranges.map((_, i) => (
                  <option key={i} value={i}>
                    {t('colorV3.rangeNumber', { defaultValue: 'Range {{number}}', number: i + 1 })}
                  </option>
                ))}
              </select>
            </label>
            {rangeSlider('center', 0, t('colorV3.centerHue', { defaultValue: 'Target hue' }), 0, 360, 1)}
            {rangeSlider('width', 0, t('colorV3.hueWidth', { defaultValue: 'Hue reach' }), 1, 180, 1)}
            {rangeSlider('center', 1, t('colorV3.centerChroma', { defaultValue: 'Target chroma' }), 0, 1, 0.01)}
            {rangeSlider('width', 1, t('colorV3.chromaWidth', { defaultValue: 'Chroma reach' }), 0.01, 1, 0.01)}
            {rangeSlider('center', 2, t('colorV3.centerLightness', { defaultValue: 'Target lightness' }), 0, 2, 0.01)}
            {rangeSlider('width', 2, t('colorV3.lightnessWidth', { defaultValue: 'Lightness reach' }), 0.01, 2, 0.01)}
            {rangeSlider('adjustment', 0, t('colorV3.bandHue', { defaultValue: 'Hue shift' }), -60, 60, 1)}
            {rangeSlider('adjustment', 1, t('colorV3.bandChroma', { defaultValue: 'Chroma' }), -100, 100, 1)}
            {rangeSlider('adjustment', 2, t('colorV3.bandLightness', { defaultValue: 'Lightness' }), -100, 100, 1)}
          </>
        )}
      </details>
    </>
  );
}
