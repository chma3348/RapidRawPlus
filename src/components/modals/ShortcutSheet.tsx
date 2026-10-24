import { useEffect } from 'react';
import { create } from 'zustand';
import { useTranslation } from 'react-i18next';
import { Keyboard, X } from 'lucide-react';

import { useSettingsStore } from '../../store/useSettingsStore';
import { formatKeyCode, KEYBIND_DEFINITIONS, KEYBIND_SECTIONS } from '../../utils/keyboardUtils';

/**
 * Every keyboard shortcut, as currently set, on one card: ⌘/ (or ?) shows
 * and hides it, as Lightroom's own shortcut sheet does. Keys changed in
 * Settings show as changed.
 */
export const useShortcutSheet = create<{ open: boolean; setOpen: (open: boolean) => void }>((set) => ({
  open: false,
  setOpen: (open) => set({ open }),
}));

export default function ShortcutSheet() {
  const { t } = useTranslation();
  const { open, setOpen } = useShortcutSheet();
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const keybinds = useSettingsStore((s) => (s.appSettings as { keybinds?: Record<string, string[]> } | null)?.keybinds);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopImmediatePropagation();
        setOpen(false);
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [open, setOpen]);

  if (!open) return null;

  const show = (combo: string[]) =>
    combo.map((k) => formatKeyCode(k, osPlatform)).join(osPlatform === 'macos' ? '' : '+');
  // Keys that are not settings: they always work.
  const fixed: [string, string][] = [
    [
      t('shortcuts.spaceKey', { defaultValue: 'Space' }),
      t('shortcuts.zoomToggle', { defaultValue: 'Zoom in and back out' }),
    ],
    ['← →', t('shortcuts.prevNext', { defaultValue: 'Previous, next photo' })],
    ['Esc', t('shortcuts.escape', { defaultValue: 'Leave a tool, then go back to the grid' })],
  ];

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/45" onClick={() => setOpen(false)}>
      <div
        className="max-h-[85vh] w-[860px] max-w-[94vw] overflow-y-auto rounded-xl bg-surface p-6 text-sm text-text-primary shadow-xl custom-scrollbar"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <span className="flex items-center gap-2 text-base font-medium">
            <Keyboard size={18} />
            {t('shortcuts.title', { defaultValue: 'Keyboard shortcuts' })}
          </span>
          <div className="flex items-center gap-3">
            <span className="text-xs text-text-secondary">
              {t('shortcuts.change', { defaultValue: 'Change them in Settings → Keyboard shortcuts' })}
            </span>
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="rounded p-1 text-text-secondary hover:bg-card-active"
            >
              <X size={16} />
            </button>
          </div>
        </div>
        <div className="columns-2 gap-8">
          {KEYBIND_SECTIONS.map((section) => {
            const defs = KEYBIND_DEFINITIONS.filter((d) => d.section === section.id);
            if (defs.length === 0) return null;
            return (
              <div key={section.id} className="mb-5 break-inside-avoid">
                <div className="mb-1.5 text-xs font-semibold uppercase tracking-wide text-text-secondary">
                  {t(section.label, { defaultValue: section.id })}
                </div>
                <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1">
                  {defs.map((d) => {
                    const own = keybinds?.[d.action];
                    const combos = own && own.length > 0 ? [own] : [d.defaultCombo, ...(d.aliases ?? [])];
                    return (
                      <div key={d.action} className="contents">
                        <dt className="truncate text-text-secondary">{t(d.description, { defaultValue: d.action })}</dt>
                        <dd className="flex justify-end gap-1">
                          {combos
                            .filter((c) => c.length > 0)
                            .map((c) => (
                              <kbd
                                key={c.join('+')}
                                className="rounded border border-border-color bg-bg-primary px-1.5 py-0.5 font-sans text-[11px] text-text-primary"
                              >
                                {show(c)}
                              </kbd>
                            ))}
                        </dd>
                      </div>
                    );
                  })}
                </dl>
              </div>
            );
          })}
          <div className="mb-5 break-inside-avoid">
            <div className="mb-1.5 text-xs font-semibold uppercase tracking-wide text-text-secondary">
              {t('shortcuts.always', { defaultValue: 'Always' })}
            </div>
            <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1">
              {fixed.map(([key, what]) => (
                <div key={key} className="contents">
                  <dt className="text-text-secondary">{what}</dt>
                  <dd className="flex justify-end">
                    <kbd className="rounded border border-border-color bg-bg-primary px-1.5 py-0.5 font-sans text-[11px] text-text-primary">
                      {key}
                    </kbd>
                  </dd>
                </div>
              ))}
            </dl>
          </div>
        </div>
      </div>
    </div>
  );
}
