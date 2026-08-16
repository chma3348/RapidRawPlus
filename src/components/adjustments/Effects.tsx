import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { Trash2 } from 'lucide-react';
import { confirm } from '@tauri-apps/plugin-dialog';
import Slider from '../ui/Slider';
import Dropdown from '../ui/Dropdown';
import { Adjustments, Effect, CreativeAdjustment } from '../../utils/adjustments';
import LUTControl from '../ui/LUTControl';
import { AppSettings } from '../ui/AppProperties';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import FlatFieldProfileModal from '../modals/FlatFieldProfileModal';

const NEW_FLAT_PROFILE = '__new_flat_profile__';

interface EffectsPanelProps {
  adjustments: Adjustments;
  isForMask: boolean;
  setAdjustments(adjustments: Partial<Adjustments>): any;
  handleLutSelect(path: string): void;
  onLutHover?: (path: string | null) => void;
  appSettings: AppSettings | null;
  onDragStateChange?: (isDragging: boolean) => void;
}

export default function EffectsPanel({
  adjustments,
  setAdjustments,
  isForMask = false,
  handleLutSelect,
  onLutHover,
  appSettings,
  onDragStateChange,
}: EffectsPanelProps) {
  const { t } = useTranslation();
  const [lutPresets, setLutPresets] = useState<Array<any>>([]);
  const [flatProfiles, setFlatProfiles] = useState<Array<any>>([]);
  const [isFlatModalOpen, setIsFlatModalOpen] = useState(false);
  useEffect(() => {
    if (isForMask) return;
    invoke('list_managed_luts')
      .then((l: any) => setLutPresets(l || []))
      .catch(() => setLutPresets([]));
    invoke('list_flat_profiles')
      .then((p: any) => setFlatProfiles(p || []))
      .catch(() => setFlatProfiles([]));
  }, [isForMask]);

  const activeFlatProfile = flatProfiles.find((p) => p.name === adjustments.flatFieldProfile);

  const handleFlatProfileSelect = (value: string) => {
    if (value === NEW_FLAT_PROFILE) {
      setIsFlatModalOpen(true);
      return;
    }
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      flatFieldProfile: value || null,
    }));
  };

  const handleFlatProfileCreated = (profile: any) => {
    invoke('list_flat_profiles')
      .then((p: any) => setFlatProfiles(p || []))
      .catch(() => {});
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      flatFieldProfile: profile?.name || null,
    }));
  };

  const handleFlatProfileDelete = async () => {
    const name = adjustments.flatFieldProfile;
    if (!name) return;
    const ok = await confirm(t('adjustments.effects.flatFieldDeleteConfirm', { name }));
    if (!ok) return;
    try {
      await invoke('delete_flat_profile', { name });
    } catch (e) {
      console.error('[flat] delete_flat_profile failed:', e);
    }
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, flatFieldProfile: null }));
    invoke('list_flat_profiles')
      .then((p: any) => setFlatProfiles(p || []))
      .catch(() => {});
  };

  // Picking a preset loads the cube AND sets its input space, so film-sim
  // LUTs (F-Log2C) render through the correct transform automatically.
  const handlePresetSelect = (path: string) => {
    const preset = lutPresets.find((p) => p.path === path);
    if (!preset) return;
    handleLutSelect(preset.path);
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      lutInputSpace: preset.inputSpace,
    }));
  };

  const handleAdjustmentChange = (key: string, value: string) => {
    const numericValue = parseInt(value, 10);
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, [key]: numericValue }));
  };

  const handleLutIntensityChange = (intensity: number) => {
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, lutIntensity: intensity }));
  };

  const handleLutClear = () => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      lutPath: null,
      lutName: null,
      lutData: null,
      lutSize: 0,
      lutIntensity: 100,
      lutInputSpace: 'display',
    }));
  };

  const adjustmentVisibility = appSettings?.adjustmentVisibility || {};

  return (
    <div className="space-y-4">
      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-2">
          {t('adjustments.effects.creative')}
        </Text>

        <Slider
          label={t('adjustments.effects.glow')}
          max={100}
          min={0}
          onChange={(e: any) => handleAdjustmentChange(CreativeAdjustment.GlowAmount, e.target.value)}
          step={1}
          value={adjustments.glowAmount}
          onDragStateChange={onDragStateChange}
        />

        <Slider
          label={t('adjustments.effects.halation')}
          max={100}
          min={0}
          onChange={(e: any) => handleAdjustmentChange(CreativeAdjustment.HalationAmount, e.target.value)}
          step={1}
          value={adjustments.halationAmount}
          onDragStateChange={onDragStateChange}
        />

        {!isForMask && (
          <Slider
            label={t('adjustments.effects.filmSaturation')}
            max={100}
            min={0}
            onChange={(e: any) => handleAdjustmentChange(CreativeAdjustment.FilmSaturation, e.target.value)}
            step={1}
            value={adjustments.filmSaturation ?? 0}
            onDragStateChange={onDragStateChange}
          />
        )}

        {!isForMask && (
          <Slider
            label={t('adjustments.effects.lightFlares')}
            max={100}
            min={0}
            onChange={(e: any) => handleAdjustmentChange(CreativeAdjustment.FlareAmount, e.target.value)}
            step={1}
            value={adjustments.flareAmount}
            onDragStateChange={onDragStateChange}
          />
        )}
      </div>

      {!isForMask && (
        <div className="space-y-4">
          <div className="p-2 bg-bg-tertiary rounded-md">
            <Text variant={TextVariants.heading} className="mb-2">
              {t('adjustments.effects.lut')}
            </Text>
            {lutPresets.length > 0 && (
              <div className="mb-3">
                <Text variant={TextVariants.small} className="mb-1 block">
                  {t('adjustments.effects.filmSimulations')}
                </Text>
                <Dropdown
                  options={lutPresets.map((p) => ({
                    label: p.inputSpace === 'flog2c' ? `${p.name} (Fujifilm)` : p.name,
                    value: p.path,
                  }))}
                  value={adjustments.lutPath || ''}
                  onChange={(v: string) => handlePresetSelect(v)}
                />
              </div>
            )}
            {adjustments.lutPath && adjustments.lutInputSpace === 'flog2c' && (
              <div className="mb-3">
                <Slider
                  label={t('adjustments.effects.simExposure')}
                  min={-3}
                  max={3}
                  step={0.05}
                  defaultValue={0}
                  value={adjustments.lutSimExposure ?? 0}
                  onChange={(e: any) =>
                    setAdjustments((prev: Partial<Adjustments>) => ({
                      ...prev,
                      lutSimExposure: parseFloat(e.target.value),
                    }))
                  }
                  onDragStateChange={onDragStateChange}
                />
              </div>
            )}
            <LUTControl
              lutPath={adjustments.lutPath || null}
              lutName={adjustments.lutName || null}
              lutIntensity={adjustments.lutIntensity || 100}
              onLutSelect={handleLutSelect}
              onLutHover={onLutHover}
              onIntensityChange={handleLutIntensityChange}
              onClear={handleLutClear}
              onDragStateChange={onDragStateChange}
            />
          </div>

          {adjustmentVisibility.vignette !== false && (
            <div className="p-2 bg-bg-tertiary rounded-md">
              <Text variant={TextVariants.heading} className="mb-2">
                {t('adjustments.effects.vignette')}
              </Text>
              <Slider
                label={t('adjustments.effects.amount')}
                max={100}
                min={-100}
                onChange={(e: any) => handleAdjustmentChange(Effect.VignetteAmount, e.target.value)}
                step={1}
                value={adjustments.vignetteAmount}
                onDragStateChange={onDragStateChange}
              />
              <Slider
                defaultValue={50}
                label={t('adjustments.effects.midpoint')}
                max={100}
                min={0}
                onChange={(e: any) => handleAdjustmentChange(Effect.VignetteMidpoint, e.target.value)}
                step={1}
                value={adjustments.vignetteMidpoint}
                onDragStateChange={onDragStateChange}
                fillOrigin="min"
              />
              <Slider
                label={t('adjustments.effects.roundness')}
                max={100}
                min={-100}
                onChange={(e: any) => handleAdjustmentChange(Effect.VignetteRoundness, e.target.value)}
                step={1}
                value={adjustments.vignetteRoundness}
                onDragStateChange={onDragStateChange}
              />
              <Slider
                defaultValue={50}
                label={t('adjustments.effects.feather')}
                max={100}
                min={0}
                onChange={(e: any) => handleAdjustmentChange(Effect.VignetteFeather, e.target.value)}
                step={1}
                value={adjustments.vignetteFeather}
                onDragStateChange={onDragStateChange}
                fillOrigin="min"
              />
            </div>
          )}

          {adjustmentVisibility.grain !== false && (
            <div className="p-2 bg-bg-tertiary rounded-md">
              <Text variant={TextVariants.heading} className="mb-2">
                {t('adjustments.effects.grain')}
              </Text>
              <Slider
                label={t('adjustments.effects.amount')}
                max={100}
                min={0}
                onChange={(e: any) => handleAdjustmentChange(Effect.GrainAmount, e.target.value)}
                step={1}
                value={adjustments.grainAmount}
                onDragStateChange={onDragStateChange}
              />
              <Slider
                defaultValue={25}
                label={t('adjustments.effects.size')}
                max={100}
                min={0}
                onChange={(e: any) => handleAdjustmentChange(Effect.GrainSize, e.target.value)}
                step={1}
                value={adjustments.grainSize}
                onDragStateChange={onDragStateChange}
                fillOrigin="min"
              />
              <Slider
                defaultValue={50}
                label={t('adjustments.effects.roughness')}
                max={100}
                min={0}
                onChange={(e: any) => handleAdjustmentChange(Effect.GrainRoughness, e.target.value)}
                step={1}
                value={adjustments.grainRoughness}
                onDragStateChange={onDragStateChange}
                fillOrigin="min"
              />
            </div>
          )}

          <div className="p-2 bg-bg-tertiary rounded-md">
            <Text variant={TextVariants.heading} className="mb-2">
              {t('adjustments.effects.flatField')}
            </Text>
            <div className="mb-1">
              <Text variant={TextVariants.small} className="mb-1 block">
                {t('adjustments.effects.flatFieldProfile')}
              </Text>
              <div className="flex items-center gap-2">
                <div className="flex-1 min-w-0">
                  <Dropdown
                    options={[
                      { label: t('adjustments.effects.flatFieldNone'), value: '' },
                      ...flatProfiles.map((p) => ({ label: p.name, value: p.name })),
                      { label: t('adjustments.effects.flatFieldNew'), value: NEW_FLAT_PROFILE },
                    ]}
                    value={adjustments.flatFieldProfile || ''}
                    onChange={(v: string) => handleFlatProfileSelect(v)}
                  />
                </div>
                {adjustments.flatFieldProfile && (
                  <button
                    className="p-1.5 text-text-secondary hover:text-red-400 transition-colors shrink-0"
                    onClick={handleFlatProfileDelete}
                    title={t('adjustments.effects.flatFieldDelete')}
                  >
                    <Trash2 size={14} />
                  </button>
                )}
              </div>
            </div>
            {activeFlatProfile && (
              <Text variant={TextVariants.small} className="mb-2 block text-text-secondary">
                {t('adjustments.effects.flatFieldStats', {
                  stops: activeFlatProfile.falloffStops ?? '?',
                  frames: activeFlatProfile.frames ?? '?',
                  date: activeFlatProfile.createdAt ?? '',
                })}
              </Text>
            )}
            {adjustments.flatFieldProfile && (
              <>
                <Slider
                  defaultValue={100}
                  label={t('adjustments.effects.amount')}
                  max={100}
                  min={0}
                  onChange={(e: any) => handleAdjustmentChange('flatFieldStrength', e.target.value)}
                  step={1}
                  value={adjustments.flatFieldStrength ?? 100}
                  onDragStateChange={onDragStateChange}
                  fillOrigin="min"
                />
                <Text variant={TextVariants.small} className="mt-1 block text-text-secondary">
                  {t('adjustments.effects.flatFieldHint')}
                </Text>
              </>
            )}
          </div>
        </div>
      )}

      <FlatFieldProfileModal
        isOpen={isFlatModalOpen}
        onClose={() => setIsFlatModalOpen(false)}
        onCreated={handleFlatProfileCreated}
      />
    </div>
  );
}
