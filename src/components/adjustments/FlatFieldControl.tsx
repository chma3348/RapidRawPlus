import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { Trash2 } from 'lucide-react';
import { confirm } from '@tauri-apps/plugin-dialog';
import Slider from '../ui/Slider';
import Dropdown from '../ui/Dropdown';
import { Adjustments } from '../../utils/adjustments';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import FlatFieldProfileModal from '../modals/FlatFieldProfileModal';

const NEW_FLAT_PROFILE = '__new_flat_profile__';

/** Flat-field profile picker, shared by both engines' panels. */
export default function FlatFieldControl({
  adjustments,
  setAdjustments,
  onDragStateChange,
}: {
  adjustments: Adjustments;
  setAdjustments(adjustments: any): any;
  onDragStateChange?: (isDragging: boolean) => void;
}) {
  const { t } = useTranslation();
  const [flatProfiles, setFlatProfiles] = useState<Array<any>>([]);
  const [isFlatModalOpen, setIsFlatModalOpen] = useState(false);
  useEffect(() => {
    invoke('list_flat_profiles')
      .then((p: any) => setFlatProfiles(p || []))
      .catch(() => setFlatProfiles([]));
  }, []);
  const handleAdjustmentChange = (key: string, value: string) => {
    const numericValue = parseInt(value, 10);
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, [key]: numericValue }));
  };

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

  return (
    <>
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
      <FlatFieldProfileModal
        isOpen={isFlatModalOpen}
        onClose={() => setIsFlatModalOpen(false)}
        onCreated={handleFlatProfileCreated}
      />
    </>
  );
}
