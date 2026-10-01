import { useEffect, useRef, useState, type ReactNode } from 'react';
import { LayoutTemplate, RotateCcw } from 'lucide-react';
import clsx from 'clsx';
import { useTranslation } from 'react-i18next';
import { useEditorLayout } from '../../../hooks/useEditorLayout';
import { useUIStore } from '../../../store/useUIStore';

/**
 * The editor's Layout menu: how the adjustments are shown, which side the
 * panel docks on, panel labels, the filmstrip, and putting it all back.
 * Everything here is remembered between sessions.
 */
export default function EditorLayoutMenu() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const [layout, update, reset] = useEditorLayout();
  const filmstrip = useUIStore((s) => s.uiVisibility.filmstrip);
  const setUI = useUIStore((s) => s.setUI);

  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const escape = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    window.addEventListener('pointerdown', close);
    window.addEventListener('keydown', escape);
    return () => {
      window.removeEventListener('pointerdown', close);
      window.removeEventListener('keydown', escape);
    };
  }, [open]);

  const segmented = <T extends string>(value: T, options: Array<[T, string]>, onChange: (v: T) => void) => (
    <div className="flex rounded-md bg-bg-primary p-0.5">
      {options.map(([v, label]) => (
        <button
          key={v}
          type="button"
          onClick={() => onChange(v)}
          aria-pressed={value === v}
          className={clsx(
            'flex-1 rounded px-2 py-1 text-xs transition-colors',
            value === v ? 'bg-card-active text-text-primary' : 'text-text-secondary hover:text-text-primary',
          )}
        >
          {label}
        </button>
      ))}
    </div>
  );
  const row = (label: string, control: ReactNode) => (
    <div className="flex flex-col gap-1.5">
      <span className="text-[11px] font-semibold uppercase tracking-[0.08em] text-text-secondary">{label}</span>
      {control}
    </div>
  );
  const onOff = (value: boolean, onChange: (v: boolean) => void) =>
    segmented(
      value ? 'on' : 'off',
      [
        ['on', t('editor.layout.show', { defaultValue: 'Show' })],
        ['off', t('editor.layout.hide', { defaultValue: 'Hide' })],
      ],
      (v) => onChange(v === 'on'),
    );

  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className={clsx(
          'p-2 rounded-full transition-colors',
          open ? 'bg-card-active text-text-primary' : 'bg-surface text-text-primary hover:bg-card-active',
        )}
        data-tooltip={t('editor.layout.tooltip', { defaultValue: 'Layout' })}
      >
        <LayoutTemplate size={20} />
      </button>
      {open && (
        <div className="absolute left-0 top-full z-50 mt-3 flex w-64 flex-col gap-4 rounded-lg border border-text-secondary/10 bg-surface/95 p-4 shadow-xl backdrop-blur-md">
          {row(
            t('editor.layout.adjustments', { defaultValue: 'Adjustments' }),
            segmented(
              layout.adjustmentsMode,
              [
                ['basic', t('editor.adjustments.modeBasic', { defaultValue: 'Basic' })],
                ['advanced', t('editor.adjustments.modeAdvanced', { defaultValue: 'Advanced' })],
              ],
              (v) => update({ adjustmentsMode: v }),
            ),
          )}
          {row(
            t('editor.layout.panelSide', { defaultValue: 'Panel side' }),
            segmented(
              layout.panelSide,
              [
                ['left', t('editor.layout.left', { defaultValue: 'Left' })],
                ['right', t('editor.layout.right', { defaultValue: 'Right' })],
              ],
              (v) => update({ panelSide: v }),
            ),
          )}
          {row(
            t('editor.layout.panelLabels', { defaultValue: 'Panel labels' }),
            onOff(layout.showRailLabels, (v) => update({ showRailLabels: v })),
          )}
          {row(
            t('editor.layout.filmstrip', { defaultValue: 'Filmstrip' }),
            onOff(filmstrip, (v) => setUI((s) => ({ uiVisibility: { ...s.uiVisibility, filmstrip: v } }))),
          )}
          {layout.hiddenSections.length > 0 && (
            <button
              type="button"
              onClick={() => update({ hiddenSections: [] })}
              className="rounded-md bg-bg-primary px-2 py-1.5 text-xs text-text-primary hover:bg-card-active"
            >
              {t('editor.layout.showHidden', {
                defaultValue: 'Show {{count}} hidden sections',
                count: layout.hiddenSections.length,
              })}
            </button>
          )}
          <button
            type="button"
            onClick={() => {
              reset();
              setUI((s) => ({ uiVisibility: { ...s.uiVisibility, filmstrip: true } }));
            }}
            className="flex items-center justify-center gap-1.5 rounded-md px-2 py-1.5 text-xs text-text-secondary hover:bg-bg-primary hover:text-text-primary"
          >
            <RotateCcw size={12} />
            {t('editor.layout.reset', { defaultValue: 'Reset layout' })}
          </button>
        </div>
      )}
    </div>
  );
}
