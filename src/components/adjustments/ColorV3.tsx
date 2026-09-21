import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import ColorV3Advanced from './ColorV3Advanced';
import { useEditorStore } from '../../store/useEditorStore';
import { defaultV3Controls, defaultV3Detail, defaultV3Effects, V3Controls, V3Detail, V3Effects } from '../../utils/colorV3';

export function ColorV3Switch({
  adjustments,
  setAdjustments,
}: {
  adjustments: any;
  setAdjustments: (fn: any) => void;
}) {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const active = adjustments.processVersion === 3;
  const change = async () => {
    setError('');
    if (active) {
      setAdjustments((prev: any) => ({ ...prev, processVersion: prev.v3PreviousVersion ?? 2 }));
      return;
    }
    if (!selectedImage) return;
    if (adjustments.lutPath || adjustments.flatFieldProfile) {
      setError(
        t('colorV3.incompatibleLut', {
          defaultValue: 'Remove the legacy LUT or flat-field profile before trying v3.',
        }),
      );
      return;
    }
    const path = selectedImage.path;
    setPending(true);
    try {
      await invoke('prepare_color_v3', { path });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      setAdjustments((prev: any) => ({
        ...prev,
        processVersion: 3,
        v3PreviousVersion: prev.processVersion ?? 2,
        v3: prev.v3 ?? defaultV3Controls(),
      }));
    } catch (e) {
      setError(String(e));
    } finally {
      setPending(false);
    }
  };
  return (
    <div className="mb-3 border border-surface rounded-md p-3 text-sm text-text-primary">
      <div className="flex items-center justify-between gap-2">
        <span>{t('colorV3.title', { defaultValue: 'Color engine v3' })}</span>
        <button
          type="button"
          disabled={pending || !selectedImage}
          onClick={change}
          aria-pressed={active}
          className="rounded-md px-2 py-1 bg-surface hover:bg-card-active focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-50"
        >
          {pending
            ? t('colorV3.checking', { defaultValue: 'Checking image…' })
            : active
              ? t('colorV3.return', { defaultValue: 'Return to previous engine' })
              : t('colorV3.try', { defaultValue: 'Try experimental v3' })}
        </button>
      </div>
      <p className="mt-2 leading-relaxed">
        {t('colorV3.explanation', {
          defaultValue:
            'V3 uses separate color settings. Your previous color edits are preserved when you switch back. Crop and mask shapes are shared.',
        })}
      </p>
      {active && (
        <p className="mt-2 leading-relaxed">
          {t('colorV3.limitsLut', {
            defaultValue: 'Legacy LUTs and flat-field profiles are not available in this mode.',
          })}
        </p>
      )}
      {error && (
        <p role="alert" className="mt-2">
          {error}
        </p>
      )}
    </div>
  );
}

export default function ColorV3Controls({
  adjustments,
  setAdjustments,
  onDragStateChange,
  showDetail = true,
  showEffects = true,
}: {
  adjustments: any;
  setAdjustments: (fn: any) => void;
  onDragStateChange?: (v: boolean) => void;
  /** Lets a host hide the detail section; every current host shows it. */
  showDetail?: boolean;
  /** Vignette and grain describe the whole frame, so masks do not offer them. */
  showEffects?: boolean;
}) {
  const { t } = useTranslation();
  const values: V3Controls = { ...defaultV3Controls(), ...adjustments.v3 };
  const renderError = useEditorStore((s) => s.colorV3Error);
  const selectedPath = useEditorStore((s) => s.selectedImage?.path);
  const [band, setBand] = useState(0);
  const [wheel, setWheel] = useState(0);
  const update = (key: keyof V3Controls, value: any) =>
    setAdjustments((prev: any) => ({ ...prev, v3: { ...defaultV3Controls(), ...prev.v3, [key]: value } }));
  const slider = (key: keyof V3Controls, label: string, min = -100, max = 100, step = 1) => (
    <Slider
      key={key}
      label={label}
      value={values[key] as number}
      min={min}
      max={max}
      step={step}
      defaultValue={key === 'pivot' ? 0.18 : 0}
      onChange={(e: any) => update(key, Number(e.target.value))}
      onDragStateChange={onDragStateChange}
    />
  );
  const detail: V3Detail = { ...defaultV3Detail(), ...values.detail };
  const detailSlider = (key: keyof V3Detail, label: string, min = -100, max = 100, fallback = 0) => (
    <Slider
      key={`detail-${key}`}
      label={label}
      value={detail[key]}
      min={min}
      max={max}
      step={1}
      defaultValue={fallback}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) => update('detail', { ...detail, [key]: Number(e.target.value) })}
    />
  );
  const effects: V3Effects = { ...defaultV3Effects(), ...values.effects };
  const effectSlider = (key: keyof V3Effects, label: string, min: number, max: number) => (
    <Slider
      key={`effect-${key}`}
      label={label}
      value={effects[key]}
      min={min}
      max={max}
      step={1}
      defaultValue={defaultV3Effects()[key]}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) => update('effects', { ...effects, [key]: Number(e.target.value) })}
    />
  );
  const bands = ['Red', 'Orange', 'Yellow', 'Green', 'Aqua', 'Blue', 'Purple', 'Magenta'];
  const wheels = ['Global', 'Shadows', 'Midtones', 'Highlights'];
  const arraySlider = (
    key: 'bands' | 'grading',
    index: number,
    column: number,
    label: string,
    min: number,
    max: number,
  ) => (
    <Slider
      key={`${key}-${index}-${column}`}
      label={label}
      min={min}
      max={max}
      value={values[key][index][column]}
      defaultValue={0}
      step={1}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) =>
        update(
          key,
          values[key].map((v, i) => (i === index ? v.map((c, j) => (j === column ? Number(e.target.value) : c)) : v)),
        )
      }
    />
  );
  return (
    <div className="flex flex-col gap-2">
      {renderError && (
        <p role="alert" className="border border-surface rounded-md p-3 text-sm text-text-primary">
          {t('colorV3.failed', { defaultValue: 'V3 could not render this edit. The displayed image may be outdated.' })}{' '}
          {renderError}
        </p>
      )}
      <h3 className="text-sm font-medium text-text-primary">{t('colorV3.tone', { defaultValue: 'Light and tone' })}</h3>
      {slider('exposure', t('colorV3.exposure', { defaultValue: 'Exposure (stops)' }), -5, 5, 0.01)}
      {slider('temperature', t('colorV3.temperature', { defaultValue: 'Warmth' }))}
      {slider('tint', t('colorV3.tint', { defaultValue: 'Tint' }))}
      {slider('contrast', t('colorV3.contrast', { defaultValue: 'Contrast' }))}
      {slider('pivot', t('colorV3.pivot', { defaultValue: 'Contrast pivot' }), 0.01, 1, 0.01)}
      {slider('shadows', t('colorV3.shadows', { defaultValue: 'Shadows' }))}
      {slider('highlights', t('colorV3.highlights', { defaultValue: 'Highlights' }))}
      {slider('blacks', t('colorV3.blacks', { defaultValue: 'Blacks' }))}
      {slider('whites', t('colorV3.whites', { defaultValue: 'Whites' }))}
      <ColorV3Advanced
        values={values}
        update={update}
        onDragStateChange={onDragStateChange}
        inspection={
          adjustments.processVersion === 3 && selectedPath ? { path: selectedPath, edits: adjustments } : undefined
        }
      />
      <h3 className="mt-3 text-sm font-medium text-text-primary">{t('colorV3.color', { defaultValue: 'Color' })}</h3>
      {slider('saturation', t('colorV3.saturation', { defaultValue: 'Saturation' }))}
      {slider('vibrance', t('colorV3.vibrance', { defaultValue: 'Vibrance' }))}
      {slider('hue', t('colorV3.hue', { defaultValue: 'Hue rotation' }), -180, 180)}
      <label className="mt-3 text-sm text-text-primary">
        {t('colorV3.selective', { defaultValue: 'Selective color' })}
        <select
          className="mt-2 w-full bg-surface text-text-primary rounded-md p-2"
          value={band}
          onChange={(e) => setBand(Number(e.target.value))}
        >
          {bands.map((b, i) => (
            <option value={i} key={b}>
              {t(`colorV3.band.${b}`, { defaultValue: b })}
            </option>
          ))}
        </select>
      </label>
      {arraySlider('bands', band, 0, t('colorV3.bandHue', { defaultValue: 'Hue shift' }), -60, 60)}
      {arraySlider('bands', band, 1, t('colorV3.bandChroma', { defaultValue: 'Chroma' }), -100, 100)}
      {arraySlider('bands', band, 2, t('colorV3.bandLightness', { defaultValue: 'Lightness' }), -100, 100)}
      <label className="mt-3 text-sm text-text-primary">
        {t('colorV3.grading', { defaultValue: 'Color grading' })}
        <select
          className="mt-2 w-full bg-surface text-text-primary rounded-md p-2"
          value={wheel}
          onChange={(e) => setWheel(Number(e.target.value))}
        >
          {wheels.map((w, i) => (
            <option value={i} key={w}>
              {t(`colorV3.wheel.${w}`, { defaultValue: w })}
            </option>
          ))}
        </select>
      </label>
      {arraySlider('grading', wheel, 0, t('colorV3.gradingHue', { defaultValue: 'Tint hue' }), 0, 360)}
      {arraySlider('grading', wheel, 1, t('colorV3.gradingAmount', { defaultValue: 'Tint amount' }), 0, 100)}
      {arraySlider('grading', wheel, 2, t('colorV3.bandLightness', { defaultValue: 'Lightness' }), -100, 100)}
      {showDetail && (
        <>
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.detail', { defaultValue: 'Detail' })}
          </h3>
          {detailSlider('sharpening', t('colorV3.sharpening', { defaultValue: 'Sharpening' }))}
          {detailSlider('threshold', t('colorV3.threshold', { defaultValue: 'Sharpening threshold' }), 0, 80, 15)}
          {detailSlider('texture', t('colorV3.texture', { defaultValue: 'Texture' }))}
          {detailSlider('clarity', t('colorV3.clarity', { defaultValue: 'Clarity' }))}
          {detailSlider('structure', t('colorV3.structure', { defaultValue: 'Structure' }))}
          {detailSlider('luminance_noise', t('colorV3.luminanceNoise', { defaultValue: 'Noise reduction' }), 0, 100)}
          {detailSlider('color_noise', t('colorV3.colorNoise', { defaultValue: 'Color noise reduction' }), 0, 100)}
          <p className="text-xs text-text-secondary leading-relaxed">
            {t('colorV3.sharpeningZoom', {
              defaultValue: 'Sharpening works at the scale of single pixels, so judge it at 100% zoom.',
            })}
          </p>
        </>
      )}
      {showEffects && (
        <>
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.effects', { defaultValue: 'Vignette and grain' })}
          </h3>
          {effectSlider('vignette_amount', t('colorV3.vignette', { defaultValue: 'Vignette' }), -100, 100)}
          {effectSlider('vignette_midpoint', t('colorV3.vignetteMidpoint', { defaultValue: 'Vignette midpoint' }), 0, 100)}
          {effectSlider('vignette_roundness', t('colorV3.vignetteRoundness', { defaultValue: 'Vignette roundness' }), -100, 100)}
          {effectSlider('vignette_feather', t('colorV3.vignetteFeather', { defaultValue: 'Vignette feather' }), 0, 100)}
          {effectSlider('grain_amount', t('colorV3.grain', { defaultValue: 'Grain' }), 0, 100)}
          {effectSlider('grain_size', t('colorV3.grainSize', { defaultValue: 'Grain size' }), 0, 100)}
          {effectSlider('grain_roughness', t('colorV3.grainRoughness', { defaultValue: 'Grain roughness' }), 0, 100)}
        </>
      )}
    </div>
  );
}
