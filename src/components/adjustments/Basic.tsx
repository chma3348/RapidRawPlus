import Slider from '../ui/Slider';
import { Adjustments, BasicAdjustment } from '../../utils/adjustments';
import { useTranslation } from 'react-i18next';

interface BasicAdjustmentsProps {
  adjustments: Adjustments;
  setAdjustments(adjustments: Partial<Adjustments>): any;
  onDragStateChange?: (isDragging: boolean) => void;
}

export default function BasicAdjustments({ adjustments, setAdjustments, onDragStateChange }: BasicAdjustmentsProps) {
  const { t } = useTranslation();

  const handleAdjustmentChange = (key: BasicAdjustment, value: any) => {
    const numericValue = parseFloat(value);
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, [key]: numericValue }));
  };

  return (
    <div>
      {/* Exposure in stops (the photo's EV), then Brightness, the
          previous engine's midtone exposure. Rendering is always v3's own,
          so there is no tone mapper to choose. */}
      <Slider
        label={t('adjustments.basic.exposure')}
        max={5}
        min={-5}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Exposure, e.target.value)}
        step={0.01}
        value={adjustments.exposure}
        onDragStateChange={onDragStateChange}
      />
      <Slider
        label={t('adjustments.basic.contrast')}
        max={100}
        min={-100}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Contrast, e.target.value)}
        step={1}
        value={adjustments.contrast}
        onDragStateChange={onDragStateChange}
      />
      {(adjustments.contrast ?? 0) !== 0 && (
        <Slider
          label={t('adjustments.basic.contrastPivot')}
          max={100}
          min={0}
          defaultValue={50}
          onChange={(e: any) => setAdjustments((prev: any) => ({ ...prev, contrastPivot: parseFloat(e.target.value) }))}
          step={1}
          value={adjustments.contrastPivot ?? 50}
          fillOrigin="min"
          onDragStateChange={onDragStateChange}
        />
      )}
      <Slider
        label={t('adjustments.basic.highlights')}
        max={100}
        min={-100}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Highlights, e.target.value)}
        step={1}
        value={adjustments.highlights}
        onDragStateChange={onDragStateChange}
      />
      <Slider
        label={t('adjustments.basic.shadows')}
        max={100}
        min={-100}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Shadows, e.target.value)}
        step={1}
        value={adjustments.shadows}
        onDragStateChange={onDragStateChange}
      />
      <Slider
        label={t('adjustments.basic.whites')}
        max={100}
        min={-100}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Whites, e.target.value)}
        step={1}
        value={adjustments.whites}
        onDragStateChange={onDragStateChange}
      />
      <Slider
        label={t('adjustments.basic.blacks')}
        max={100}
        min={-100}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Blacks, e.target.value)}
        step={1}
        value={adjustments.blacks}
        onDragStateChange={onDragStateChange}
      />
      <Slider
        label={t('adjustments.basic.brightness', { defaultValue: 'Brightness' })}
        max={5}
        min={-5}
        onChange={(e: any) => handleAdjustmentChange(BasicAdjustment.Brightness, e.target.value)}
        step={0.01}
        value={adjustments.brightness}
        onDragStateChange={onDragStateChange}
      />
    </div>
  );
}
