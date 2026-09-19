import { Minus, Plus, RotateCcw, Sparkles } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import Slider from '../../ui/Slider';

export default function SubjectSelectionControls({
  parameters,
  onChange,
  paint = false,
  onAutoSelect,
}: {
  parameters: any;
  onChange: (parameters: any) => void;
  paint?: boolean;
  onAutoSelect?: () => void;
}) {
  const { t } = useTranslation();
  const mode = parameters.selectionMode ?? 'include';
  const hasSelection = !!parameters.maskDataBase64 || !!parameters.subjectPoints?.length || !!parameters.lines?.length;
  return (
    <div className="space-y-3">
      {!paint && onAutoSelect && (
        <button
          type="button"
          onClick={onAutoSelect}
          className="flex w-full items-center justify-center gap-2 rounded-md border border-surface px-2 py-1.5 text-sm text-text-primary hover:bg-card-active focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          <Sparkles size={14} />
          {t('editor.masks.subject.autoSelect', 'Select subject automatically')}
        </button>
      )}
      {!paint && (
        <div className="flex gap-1" role="group" aria-label={t('editor.masks.subject.mode', 'Selection mode')}>
          {(['include', 'exclude'] as const).map((value) => (
            <button
              key={value}
              type="button"
              aria-pressed={mode === value}
              disabled={value === 'exclude' && !hasSelection}
              onClick={() => onChange({ ...parameters, selectionMode: value })}
              className={`flex flex-1 items-center justify-center gap-1 rounded-md px-2 py-1.5 text-sm border
                focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent
                disabled:opacity-50 disabled:cursor-not-allowed ${
                  mode === value
                    ? 'border-accent bg-card-active text-text-primary'
                    : 'border-surface text-text-primary hover:bg-card-active'
                }`}
            >
              {value === 'include' ? <Plus size={14} /> : <Minus size={14} />}
              {value === 'include'
                ? t('editor.masks.subject.include', 'Include')
                : t('editor.masks.subject.exclude', 'Exclude')}
            </button>
          ))}
        </div>
      )}
      <p className="text-xs leading-relaxed text-text-primary">
        {paint
          ? t(
              'editor.masks.subject.paintHint',
              'Paint inside the subject to include it. Use the eraser to exclude background.',
            )
          : t(
              'editor.masks.subject.hint',
              'The subject is selected automatically. Click to include more, Shift-click or Alt-click to exclude. Drag a box to start over.',
            )}
      </p>
      {hasSelection && (
        <>
          <Slider
            label={t('editor.masks.subject.edgeBalance', 'Edge balance')}
            min={-100}
            max={100}
            step={1}
            defaultValue={0}
            value={parameters.edgeBalance ?? 0}
            onChange={(e) => onChange({ ...parameters, edgeBalance: Number(e.target.value) })}
          />
          <p className="text-xs leading-relaxed text-text-primary">
            {t(
              'editor.masks.subject.edgeHint',
              'Negative values tighten uncertain edges; positive values include more edge detail.',
            )}
          </p>
          <button
            type="button"
            onClick={() =>
              onChange({
                ...parameters,
                maskDataBase64: null,
                subjectPoints: [],
                lines: [],
                subjectRequestId: null,
                selectionMode: 'include',
                startX: undefined,
                startY: undefined,
                endX: undefined,
                endY: undefined,
              })
            }
            className="flex items-center gap-1.5 text-sm text-text-primary hover:text-accent rounded
            focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
          >
            <RotateCcw size={14} />
            {t('editor.masks.subject.startOver', 'Start over')}
          </button>
        </>
      )}
    </div>
  );
}
