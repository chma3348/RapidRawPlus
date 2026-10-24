export interface KeybindDefinition {
  action: string;
  description: string;
  defaultCombo: string[];
  /** More keys for the same action, while it keeps its default (P and Z
   *  both pick; ⌘Y and ⇧⌘Z both redo). */
  aliases?: string[][];
  /** Where it works, when only one place: actions for different places may
   *  share a key (\\ shows the original in the editor, the filter bar in the
   *  grid, as in Lightroom Classic). */
  context?: 'editor' | 'library';
  section: 'library' | 'view' | 'rating' | 'panels' | 'editing';
}

export interface KeybindSection {
  id: KeybindDefinition['section'];
  label: string;
}

export const KEYBIND_SECTIONS: KeybindSection[] = [
  { id: 'library', label: 'settings.keybinds.sections.library' },
  { id: 'editing', label: 'settings.keybinds.sections.editing' },
  { id: 'view', label: 'settings.keybinds.sections.view' },
  { id: 'rating', label: 'settings.keybinds.sections.rating' },
  { id: 'panels', label: 'settings.keybinds.sections.panels' },
];

export const KEYBIND_DEFINITIONS: KeybindDefinition[] = [
  {
    action: 'open_image',
    description: 'settings.keybinds.actions.open_image',
    defaultCombo: ['Enter'],
    section: 'library',
  },
  {
    action: 'copy_files',
    description: 'settings.keybinds.actions.copy_files',
    defaultCombo: ['ctrl', 'alt', 'KeyC'],
    section: 'library',
  },
  {
    action: 'paste_files',
    description: 'settings.keybinds.actions.paste_files',
    defaultCombo: ['ctrl', 'alt', 'KeyV'],
    section: 'library',
  },
  {
    action: 'select_all',
    description: 'settings.keybinds.actions.select_all',
    defaultCombo: ['ctrl', 'KeyA'],
    section: 'library',
  },
  {
    action: 'delete_selected',
    description: 'settings.keybinds.actions.delete_selected',
    defaultCombo: ['Delete'],
    section: 'library',
  },
  {
    action: 'preview_prev',
    description: 'settings.keybinds.actions.preview_prev',
    defaultCombo: ['ArrowLeft'],
    section: 'library',
  },
  {
    action: 'preview_next',
    description: 'settings.keybinds.actions.preview_next',
    defaultCombo: ['ArrowRight'],
    section: 'library',
  },
  {
    action: 'zoom_in_step',
    description: 'settings.keybinds.actions.zoom_in_step',
    defaultCombo: ['ArrowUp'],
    section: 'view',
  },
  {
    action: 'zoom_out_step',
    description: 'settings.keybinds.actions.zoom_out_step',
    defaultCombo: ['ArrowDown'],
    section: 'view',
  },
  {
    action: 'cycle_zoom',
    description: 'settings.keybinds.actions.cycle_zoom',
    defaultCombo: ['Space'],
    section: 'view',
  },
  {
    action: 'zoom_in',
    description: 'settings.keybinds.actions.zoom_in',
    defaultCombo: ['ctrl', 'Equal'],
    section: 'view',
  },
  {
    action: 'zoom_out',
    description: 'settings.keybinds.actions.zoom_out',
    defaultCombo: ['ctrl', 'Minus'],
    section: 'view',
  },
  {
    action: 'zoom_fit',
    description: 'settings.keybinds.actions.zoom_fit',
    defaultCombo: ['ctrl', 'shift', 'Digit0'],
    section: 'view',
  },
  {
    action: 'zoom_100',
    description: 'settings.keybinds.actions.zoom_100',
    defaultCombo: ['ctrl', 'alt', 'Digit0'],
    section: 'view',
  },
  {
    action: 'toggle_fullscreen',
    description: 'settings.keybinds.actions.toggle_fullscreen',
    defaultCombo: ['KeyF'],
    aliases: [['ctrl', 'shift', 'KeyF']],
    section: 'view',
  },
  {
    action: 'show_original',
    description: 'settings.keybinds.actions.show_original',
    defaultCombo: ['Backslash'],
    context: 'editor',
    section: 'view',
  },
  { action: 'rate_0', description: 'settings.keybinds.actions.rate_0', defaultCombo: ['Digit0'], section: 'rating' },
  { action: 'rate_1', description: 'settings.keybinds.actions.rate_1', defaultCombo: ['Digit1'], section: 'rating' },
  { action: 'rate_2', description: 'settings.keybinds.actions.rate_2', defaultCombo: ['Digit2'], section: 'rating' },
  { action: 'rate_3', description: 'settings.keybinds.actions.rate_3', defaultCombo: ['Digit3'], section: 'rating' },
  { action: 'rate_4', description: 'settings.keybinds.actions.rate_4', defaultCombo: ['Digit4'], section: 'rating' },
  { action: 'rate_5', description: 'settings.keybinds.actions.rate_5', defaultCombo: ['Digit5'], section: 'rating' },
  {
    action: 'color_label_none',
    description: 'settings.keybinds.actions.color_label_none',
    defaultCombo: ['shift', 'Digit0'],
    section: 'rating',
  },
  {
    action: 'color_label_red',
    description: 'settings.keybinds.actions.color_label_red',
    defaultCombo: ['shift', 'Digit1'],
    section: 'rating',
  },
  {
    action: 'color_label_yellow',
    description: 'settings.keybinds.actions.color_label_yellow',
    defaultCombo: ['shift', 'Digit2'],
    section: 'rating',
  },
  {
    action: 'color_label_green',
    description: 'settings.keybinds.actions.color_label_green',
    defaultCombo: ['shift', 'Digit3'],
    section: 'rating',
  },
  {
    action: 'color_label_blue',
    description: 'settings.keybinds.actions.color_label_blue',
    defaultCombo: ['shift', 'Digit4'],
    section: 'rating',
  },
  {
    action: 'color_label_purple',
    description: 'settings.keybinds.actions.color_label_purple',
    defaultCombo: ['shift', 'Digit5'],
    section: 'rating',
  },
  {
    action: 'toggle_adjustments',
    description: 'settings.keybinds.actions.toggle_adjustments',
    defaultCombo: ['KeyE'],
    section: 'panels',
  },
  {
    action: 'toggle_crop_panel',
    description: 'settings.keybinds.actions.toggle_crop_panel',
    defaultCombo: ['KeyC'],
    section: 'panels',
  },
  {
    action: 'toggle_masks',
    description: 'settings.keybinds.actions.toggle_masks',
    defaultCombo: ['KeyM'],
    section: 'panels',
  },
  {
    action: 'toggle_ai',
    description: 'settings.keybinds.actions.toggle_ai',
    defaultCombo: ['shift', 'KeyK'],
    section: 'panels',
  },
  {
    action: 'toggle_presets',
    description: 'settings.keybinds.actions.toggle_presets',
    defaultCombo: ['shift', 'KeyP'],
    section: 'panels',
  },
  {
    action: 'toggle_metadata',
    description: 'settings.keybinds.actions.toggle_metadata',
    defaultCombo: ['KeyI'],
    aliases: [['KeyK']],
    section: 'panels',
  },
  {
    action: 'toggle_analytics',
    description: 'settings.keybinds.actions.toggle_analytics',
    defaultCombo: ['ctrl', 'Digit0'],
    section: 'panels',
  },
  {
    action: 'toggle_export',
    description: 'settings.keybinds.actions.toggle_export',
    defaultCombo: ['shift', 'KeyE'],
    section: 'panels',
  },
  {
    action: 'toggle_library_exif',
    description: 'settings.keybinds.actions.toggle_library_exif',
    defaultCombo: ['KeyT'],
    section: 'library',
  },
  { action: 'undo', description: 'settings.keybinds.actions.undo', defaultCombo: ['ctrl', 'KeyZ'], section: 'editing' },
  {
    action: 'redo',
    description: 'settings.keybinds.actions.redo',
    defaultCombo: ['ctrl', 'shift', 'KeyZ'],
    aliases: [['ctrl', 'KeyY']],
    section: 'editing',
  },
  {
    action: 'copy_adjustments',
    description: 'settings.keybinds.actions.copy_adjustments',
    defaultCombo: ['ctrl', 'KeyC'],
    section: 'editing',
  },
  {
    action: 'paste_adjustments',
    description: 'settings.keybinds.actions.paste_adjustments',
    defaultCombo: ['ctrl', 'KeyV'],
    section: 'editing',
  },
  {
    action: 'rotate_left',
    description: 'settings.keybinds.actions.rotate_left',
    defaultCombo: ['ctrl', 'BracketLeft'],
    section: 'editing',
  },
  {
    action: 'rotate_right',
    description: 'settings.keybinds.actions.rotate_right',
    defaultCombo: ['ctrl', 'BracketRight'],
    section: 'editing',
  },
  {
    action: 'toggle_crop',
    description: 'settings.keybinds.actions.toggle_crop',
    defaultCombo: ['KeyS'],
    section: 'editing',
  },
  {
    action: 'brush_size_up',
    description: 'settings.keybinds.actions.brush_size_up',
    defaultCombo: ['BracketRight'],
    section: 'editing',
  },
  {
    action: 'brush_size_down',
    description: 'settings.keybinds.actions.brush_size_down',
    defaultCombo: ['BracketLeft'],
    section: 'editing',
  },
  // Culling and the library, with Lightroom's keys.
  {
    action: 'flag_pick',
    description: 'settings.keybinds.actions.flag_pick',
    defaultCombo: ['KeyZ'],
    section: 'rating',
  },
  {
    action: 'flag_reject',
    description: 'settings.keybinds.actions.flag_reject',
    defaultCombo: ['KeyX'],
    section: 'rating',
  },
  {
    action: 'flag_clear',
    description: 'settings.keybinds.actions.flag_clear',
    defaultCombo: ['KeyU'],
    section: 'rating',
  },
  {
    action: 'open_cull',
    description: 'settings.keybinds.actions.open_cull',
    defaultCombo: ['shift', 'KeyC'],
    section: 'library',
  },
  {
    action: 'cycle_folder_panel',
    description: 'settings.keybinds.actions.cycle_folder_panel',
    defaultCombo: ['shift', 'KeyF'],
    section: 'view',
  },
  {
    action: 'toggle_filter_bar',
    description: 'settings.keybinds.actions.toggle_filter_bar',
    defaultCombo: ['Backslash'],
    context: 'library',
    section: 'library',
  },
  {
    action: 'back_to_grid',
    description: 'settings.keybinds.actions.back_to_grid',
    defaultCombo: ['KeyG'],
    aliases: [['KeyP']],
    section: 'view',
  },
  // Lightroom's (the Lightroom desktop app's) own shortcuts.
  {
    action: 'auto_tone',
    description: 'settings.keybinds.actions.auto_tone',
    defaultCombo: ['shift', 'KeyA'],
    section: 'editing',
  },
  {
    action: 'toggle_clipping',
    description: 'settings.keybinds.actions.toggle_clipping',
    defaultCombo: ['KeyJ'],
    section: 'view',
  },
  {
    action: 'wb_picker',
    description: 'settings.keybinds.actions.wb_picker',
    defaultCombo: ['KeyW'],
    section: 'editing',
  },
  {
    action: 'crop_overlay_cycle',
    description: 'settings.keybinds.actions.crop_overlay_cycle',
    defaultCombo: ['KeyO'],
    section: 'editing',
  },
  {
    action: 'mask_linear',
    description: 'settings.keybinds.actions.mask_linear',
    defaultCombo: ['KeyL'],
    section: 'editing',
  },
  {
    action: 'mask_radial',
    description: 'settings.keybinds.actions.mask_radial',
    defaultCombo: ['KeyR'],
    section: 'editing',
  },
  {
    action: 'mask_brush',
    description: 'settings.keybinds.actions.mask_brush',
    defaultCombo: ['KeyB'],
    section: 'editing',
  },
  {
    action: 'heal_brush',
    description: 'settings.keybinds.actions.heal_brush',
    defaultCombo: ['KeyH'],
    section: 'editing',
  },
  {
    action: 'brush_feather_up',
    description: 'settings.keybinds.actions.brush_feather_up',
    defaultCombo: ['shift', 'BracketRight'],
    section: 'editing',
  },
  {
    action: 'brush_feather_down',
    description: 'settings.keybinds.actions.brush_feather_down',
    defaultCombo: ['shift', 'BracketLeft'],
    section: 'editing',
  },
  {
    action: 'create_version',
    description: 'settings.keybinds.actions.create_version',
    defaultCombo: ['shift', 'KeyM'],
    section: 'editing',
  },
  {
    action: 'paste_selected',
    description: 'settings.keybinds.actions.paste_selected',
    defaultCombo: ['ctrl', 'shift', 'KeyV'],
    section: 'editing',
  },
  {
    action: 'export_previous',
    description: 'settings.keybinds.actions.export_previous',
    defaultCombo: ['ctrl', 'KeyE'],
    section: 'editing',
  },
  {
    action: 'merge_hdr',
    description: 'settings.keybinds.actions.merge_hdr',
    defaultCombo: ['ctrl', 'KeyH'],
    section: 'library',
  },
  {
    action: 'merge_pano',
    description: 'settings.keybinds.actions.merge_pano',
    defaultCombo: ['ctrl', 'KeyM'],
    section: 'library',
  },
  {
    action: 'select_none',
    description: 'settings.keybinds.actions.select_none',
    defaultCombo: ['ctrl', 'KeyD'],
    section: 'library',
  },
  {
    action: 'toggle_filmstrip',
    description: 'settings.keybinds.actions.toggle_filmstrip',
    defaultCombo: ['Slash'],
    section: 'view',
  },
  {
    action: 'panel_detail',
    description: 'settings.keybinds.actions.panel_detail',
    defaultCombo: ['KeyD'],
    section: 'panels',
  },
  {
    action: 'panel_light',
    description: 'settings.keybinds.actions.panel_light',
    defaultCombo: ['ctrl', 'Digit1'],
    section: 'panels',
  },
  {
    action: 'panel_color',
    description: 'settings.keybinds.actions.panel_color',
    defaultCombo: ['ctrl', 'Digit2'],
    section: 'panels',
  },
  {
    action: 'panel_effects',
    description: 'settings.keybinds.actions.panel_effects',
    defaultCombo: ['ctrl', 'Digit3'],
    section: 'panels',
  },
  {
    action: 'panel_details',
    description: 'settings.keybinds.actions.panel_details',
    defaultCombo: ['ctrl', 'Digit4'],
    section: 'panels',
  },
  {
    action: 'panel_optics',
    description: 'settings.keybinds.actions.panel_optics',
    defaultCombo: ['ctrl', 'Digit5'],
    section: 'panels',
  },
  {
    action: 'panel_geometry',
    description: 'settings.keybinds.actions.panel_geometry',
    defaultCombo: ['ctrl', 'Digit6'],
    section: 'panels',
  },
  {
    action: 'shortcut_sheet',
    description: 'settings.keybinds.actions.shortcut_sheet',
    defaultCombo: ['ctrl', 'Slash'],
    aliases: [['shift', 'Slash']],
    section: 'view',
  },
];

const symMap: Record<string, string> = {
  Space: 'Space',
  Backspace: '⌫',
  Enter: 'Enter',
  Delete: 'Delete',
  ArrowUp: '↑',
  ArrowDown: '↓',
  ArrowLeft: '←',
  ArrowRight: '→',
  BracketLeft: '[',
  BracketRight: ']',
  Minus: '-',
  Equal: '+',
  Comma: ',',
  Period: '.',
  Slash: '/',
  Semicolon: ';',
  Quote: "'",
  Backquote: '`',
  Backslash: '\\',
  Tab: 'Tab',
  Escape: 'Esc',
  PageUp: 'Page Up',
  PageDown: 'Page Down',
  Home: 'Home',
  End: 'End',
  Insert: 'Insert',
  NumpadAdd: 'Numpad +',
  NumpadMultiply: 'Numpad *',
  NumpadDivide: 'Numpad /',
  NumpadSubtract: 'Numpad -',
  NumpadDecimal: 'Numpad .',
  NumpadComma: 'Numpad ,',
  NumpadEnter: 'Numpad Enter',
  NumpadEqual: 'Numpad =',
  CapsLock: 'Caps Lock',
  PrintScreen: 'PrtSc',
};

export function normalizeCombo(event: KeyboardEvent, osPlatform?: string): string[] {
  const isMacDelete = osPlatform === 'macos' && event.code === 'Backspace' && (event.ctrlKey || event.metaKey);
  const parts: string[] = [];
  if ((event.ctrlKey || event.metaKey) && !isMacDelete) parts.push('ctrl');
  if (event.shiftKey) parts.push('shift');
  if (event.altKey) parts.push('alt');
  let code = isMacDelete ? 'Delete' : event.code;
  if (/^Numpad[0-9]$/.test(code)) {
    code = `Digit${code.slice(-1)}`;
  } else if (code === 'NumpadAdd') {
    code = 'Equal';
  } else if (code === 'NumpadSubtract') {
    code = 'Minus';
  }
  if (isValidShortcutKey(code)) {
    parts.push(code);
  }
  return parts;
}

export function codeToDisplayLabel(code: string): string | null {
  if (/^Key[A-Z]$/.test(code) || /^Digit[0-9]$/.test(code)) {
    return code[code.length - 1].toUpperCase();
  }
  if (/^Numpad[0-9]$/.test(code)) {
    return `Numpad ${code.slice(-1)}`;
  }
  return symMap[code] ?? null;
}

export function isValidShortcutKey(code: string): boolean {
  if (code.startsWith('Key') || code.startsWith('Digit')) return true;
  if (code.startsWith('F') && /^\d+$/.test(code.slice(1))) return true;
  if (/^Numpad[0-9]$/.test(code)) return true;
  return code in symMap;
}

export function formatKeyCode(key: string, osPlatform: string): string {
  if (key === 'ctrl') return osPlatform === 'macos' ? '⌘' : 'Ctrl';
  if (key === 'shift') return 'Shift';
  if (key === 'alt') return osPlatform === 'macos' ? '⌥' : 'Alt';
  if (key === 'Delete' && osPlatform === 'macos') return 'Delete / ⌘+⌫';
  const label = codeToDisplayLabel(key);
  return label || key;
}

export function arraysEqual(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** Whether two actions bound to the same key would collide: not when each
 *  works in a different place. */
export function keybindsCollide(a: KeybindDefinition, b: KeybindDefinition): boolean {
  return !a.context || !b.context || a.context === b.context;
}
