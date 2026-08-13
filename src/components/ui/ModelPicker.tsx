import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import Dropdown, { OptionItem } from './Dropdown';
import { Invokes } from './AppProperties';

export enum ModelTaskType {
  Upscale = 'upscale',
  Deblur = 'deblur',
  Restore = 'restore',
  Mask = 'mask',
  Inpaint = 'inpaint',
}

export interface ModelInfo {
  id: string;
  displayName: string;
  taskType: string;
  available: boolean;
  builtin: boolean;
  params: Record<string, any>;
}

interface ModelPickerProps {
  className?: string;
  disabled?: boolean;
  onChange: (modelId: string, model: ModelInfo) => void;
  /** Extra filter on top of the task type, e.g. by params.mask_subtype. */
  filter?: (model: ModelInfo) => boolean;
  taskType: ModelTaskType;
  triggerClassName?: string;
  value: string | null;
}

/**
 * Reusable dropdown listing the registered models for one task type.
 * Models whose weight file is missing are shown but cannot be selected.
 */
const ModelPicker = ({
  className,
  disabled,
  onChange,
  filter,
  taskType,
  triggerClassName,
  value,
}: ModelPickerProps) => {
  const { t } = useTranslation();
  const [models, setModels] = useState<Array<ModelInfo>>([]);

  const refresh = useCallback(async () => {
    try {
      const result: Array<ModelInfo> = await invoke(Invokes.ListRegisteredModels, { taskType });
      setModels(result);
    } catch (error) {
      console.error('Failed to list registered models:', error);
      setModels([]);
    }
  }, [taskType]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const visibleModels = useMemo(() => (filter ? models.filter(filter) : models), [models, filter]);

  const options: Array<OptionItem<string>> = useMemo(
    () =>
      visibleModels.map((model) => ({
        label: model.available
          ? model.displayName
          : `${model.displayName} (${t('modelRegistry.notDownloaded')})`,
        value: model.id,
      })),
    [visibleModels, t],
  );

  const effectiveValue = useMemo(() => {
    const isSelectable = (id: string | null) =>
      id !== null && visibleModels.some((m) => m.id === id && m.available);
    if (isSelectable(value)) {
      return value;
    }
    return visibleModels.find((m) => m.available)?.id ?? null;
  }, [visibleModels, value]);

  const handleChange = (modelId: string) => {
    const model = visibleModels.find((m) => m.id === modelId);
    if (!model || !model.available) {
      return;
    }
    onChange(modelId, model);
  };

  return (
    <Dropdown
      className={className}
      disabled={disabled || options.length === 0}
      onChange={handleChange}
      options={options}
      placeholder={t('modelRegistry.noModels')}
      triggerClassName={triggerClassName}
      value={effectiveValue}
    />
  );
};

export default ModelPicker;
