import { useCallback, useMemo } from 'react';
import { useSettingsStore } from '../store/useSettingsStore';
import { defaultEditorLayout, EditorLayout, resolveEditorLayout } from '../utils/editorLayout';

/** The editor layout from settings, and a way to change and save it. */
export function useEditorLayout(): [EditorLayout, (change: Partial<EditorLayout>) => void, () => void] {
  const saved = useSettingsStore((s) => (s.appSettings as any)?.editorLayout);
  const layout = useMemo(() => resolveEditorLayout(saved), [saved]);

  const update = useCallback((change: Partial<EditorLayout>) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (!appSettings) return;
    const current = resolveEditorLayout((appSettings as any).editorLayout);
    void handleSettingsChange({ ...appSettings, editorLayout: { ...current, ...change } } as any);
  }, []);

  const reset = useCallback(() => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (!appSettings) return;
    const current = resolveEditorLayout((appSettings as any).editorLayout);
    // Keep the chosen mode; everything about arrangement goes back to default.
    void handleSettingsChange({
      ...appSettings,
      editorLayout: { ...defaultEditorLayout(), adjustmentsMode: current.adjustmentsMode },
    } as any);
  }, []);

  return [layout, update, reset];
}
