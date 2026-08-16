import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';

interface FlatFieldProfileModalProps {
  isOpen: boolean;
  onClose(): void;
  onCreated(profile: any): void;
}

export default function FlatFieldProfileModal({ isOpen, onClose, onCreated }: FlatFieldProfileModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const [name, setName] = useState('');
  const [framePaths, setFramePaths] = useState<Array<string>>([]);
  const [isBuilding, setIsBuilding] = useState(false);
  const [result, setResult] = useState<any>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (isOpen) {
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
        setName('');
        setFramePaths([]);
        setIsBuilding(false);
        setResult(null);
        setError(null);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  const handlePickFrames = useCallback(async () => {
    const selected = await open({
      multiple: true,
      title: t('modals.flatField.pickFramesTitle'),
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    setFramePaths(paths.slice(0, 20));
    setError(null);
  }, [t]);

  const handleCreate = useCallback(async () => {
    if (!name.trim() || framePaths.length === 0 || isBuilding) return;
    setIsBuilding(true);
    setError(null);
    try {
      const profile: any = await invoke('create_flat_profile', {
        name: name.trim(),
        sourcePaths: framePaths,
      });
      setResult(profile);
      onCreated(profile);
    } catch (e: any) {
      console.error('[flat] create_flat_profile failed:', e);
      setError(String(e));
    } finally {
      setIsBuilding(false);
    }
  }, [name, framePaths, isBuilding, onCreated]);

  if (!isMounted) return null;

  const warnings: Array<string> = [];
  if (result) {
    if ((result.clippedPercent ?? 0) > 0.5) {
      warnings.push(t('modals.flatField.warnClipped', { percent: result.clippedPercent }));
    }
    if ((result.falloffStops ?? 0) > 4.5) {
      warnings.push(t('modals.flatField.warnDeepFalloff', { stops: result.falloffStops }));
    }
    if ((result.frames ?? 0) < 3) {
      warnings.push(t('modals.flatField.warnFewFrames'));
    }
  }

  return (
    <div
      className={`fixed inset-0 flex items-center justify-center z-50 bg-black/30 backdrop-blur-xs transition-opacity duration-300 ease-in-out ${
        show ? 'opacity-100' : 'opacity-0'
      }`}
      onClick={isBuilding ? undefined : onClose}
      role="dialog"
    >
      <div
        className={`bg-surface rounded-lg shadow-xl p-6 w-full max-w-lg transform transition-all duration-300 ease-out ${
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'
        }`}
        onClick={(e: any) => e.stopPropagation()}
      >
        <Text variant={TextVariants.title} className="mb-4">
          {t('modals.flatField.title')}
        </Text>

        {!result ? (
          <div className="space-y-4 text-sm">
            <div>
              <Text variant={TextVariants.heading} className="block mb-2">
                {t('modals.flatField.name')}
              </Text>
              <input
                autoFocus
                className="w-full bg-bg-primary border border-surface rounded-md p-2 text-sm text-text-primary focus:ring-accent focus:border-accent"
                onChange={(e: any) => setName(e.target.value)}
                placeholder={t('modals.flatField.namePlaceholder')}
                type="text"
                value={name}
              />
            </div>

            <div>
              <button
                className="w-full px-3 py-2 bg-bg-primary border border-dashed border-surface rounded-md text-text-secondary hover:bg-card-active transition-colors text-sm"
                disabled={isBuilding}
                onClick={handlePickFrames}
              >
                {framePaths.length > 0
                  ? t('modals.flatField.framesSelected', { count: framePaths.length })
                  : t('modals.flatField.pickFrames')}
              </button>
              <p className="text-xs text-text-secondary mt-2">{t('modals.flatField.framesHint')}</p>
            </div>

            {error && <p className="text-xs text-red-400">{error}</p>}

            <div className="flex justify-end gap-3 pt-2">
              <button
                className="px-4 py-2 bg-bg-primary text-text-secondary rounded-md hover:bg-card-active transition-colors text-sm"
                disabled={isBuilding}
                onClick={onClose}
              >
                {t('modals.flatField.cancel')}
              </button>
              <button
                className="px-4 py-2 bg-accent text-button-text rounded-md hover:bg-accent-hover transition-colors text-sm disabled:opacity-50 flex items-center gap-2"
                disabled={!name.trim() || framePaths.length === 0 || isBuilding}
                onClick={handleCreate}
              >
                {isBuilding && <Loader2 size={14} className="animate-spin" />}
                {isBuilding ? t('modals.flatField.building') : t('modals.flatField.create')}
              </button>
            </div>
          </div>
        ) : (
          <div className="space-y-4 text-sm">
            <p className="text-text-primary">
              {t('modals.flatField.builtSummary', {
                name: result.name,
                stops: result.falloffStops,
                frames: result.frames,
              })}
            </p>
            {(result.clippedPercent ?? 0) <= 0.5 && (
              <p className="text-xs text-text-secondary">{t('modals.flatField.noClipping')}</p>
            )}
            {warnings.map((w, i) => (
              <p key={i} className="text-xs text-amber-400">
                {w}
              </p>
            ))}
            <div className="flex justify-end pt-2">
              <button
                className="px-4 py-2 bg-accent text-button-text rounded-md hover:bg-accent-hover transition-colors text-sm"
                onClick={onClose}
              >
                {t('modals.flatField.done')}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
