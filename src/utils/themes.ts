import { Theme } from '../components/ui/AppProperties';

export interface ThemeProps {
  cssVariables: any;
  id: Theme;
  /** Light backgrounds: some views (curves, waveform, toasts) draw differently. */
  isLight: boolean;
  name: string;
  splashImage: string;
}

// Panels sit a step above the background and controls a step above panels,
// so buttons and fields read as raised rather than recessed. The accent
// variables are set separately, from the accent the user picks.
export const THEMES: Array<ThemeProps> = [
  {
    id: Theme.Graphite,
    isLight: false,
    name: 'settings.themes.graphite',
    splashImage: '/splash-dark.jpg',
    cssVariables: {
      '--app-bg-primary': '#1B1B1D',
      '--app-bg-secondary': '#232326',
      '--app-surface': '#2E2E32',
      '--app-card-active': '#38383D',
      '--app-text-primary': '#E8E8EA',
      '--app-text-secondary': '#8E8E95',
      '--app-border-color': '#34343A',
    },
  },
  {
    id: Theme.StudioGrey,
    isLight: false,
    name: 'settings.themes.studioGrey',
    splashImage: '/splash-grey.jpg',
    cssVariables: {
      '--app-bg-primary': '#3A3A3A',
      '--app-bg-secondary': '#444444',
      '--app-surface': '#525252',
      '--app-card-active': '#5E5E5E',
      '--app-text-primary': '#F2F2F2',
      '--app-text-secondary': '#B8B8B8',
      '--app-border-color': '#585858',
    },
  },
  {
    id: Theme.Darkroom,
    isLight: false,
    name: 'settings.themes.darkroom',
    splashImage: '/splash-dark.jpg',
    cssVariables: {
      '--app-bg-primary': '#0D0D0F',
      '--app-bg-secondary': '#151517',
      '--app-surface': '#202023',
      '--app-card-active': '#2A2A2E',
      '--app-text-primary': '#D6D6D8',
      '--app-text-secondary': '#77777D',
      '--app-border-color': '#232326',
    },
  },
  {
    id: Theme.Paper,
    isLight: true,
    name: 'settings.themes.paper',
    splashImage: '/splash-light.jpg',
    cssVariables: {
      '--app-bg-primary': '#E9E9E6',
      '--app-bg-secondary': '#F5F5F3',
      '--app-surface': '#DDDDD9',
      '--app-card-active': '#D2D2CE',
      '--app-text-primary': '#1D1D1F',
      '--app-text-secondary': '#6A6A6F',
      '--app-border-color': '#D0D0CC',
    },
  },
];

export const DEFAULT_THEME_ID = Theme.Graphite;

/** Themes from before the redesign, kept working for saved settings. */
const LEGACY_THEMES: Record<string, Theme> = {
  dark: Theme.Graphite,
  grey: Theme.StudioGrey,
  light: Theme.Paper,
  snow: Theme.Paper,
  arctic: Theme.Paper,
};

export const resolveThemeId = (id?: string | null): Theme => {
  if (id && THEMES.some((t) => t.id === id)) return id as Theme;
  return (id && LEGACY_THEMES[id]) || DEFAULT_THEME_ID;
};

export const getTheme = (id?: string | null): ThemeProps =>
  THEMES.find((t) => t.id === resolveThemeId(id)) as ThemeProps;

export const isLightTheme = (id?: string | null): boolean => getTheme(id).isLight;

export interface AccentProps {
  id: string;
  name: string;
  /** Accent on the dark themes, and the text drawn on top of it. */
  dark: { color: string; text: string };
  /** A deeper shade for Paper, so it still stands out on a light background. */
  light: { color: string; text: string };
}

export const ACCENTS: Array<AccentProps> = [
  {
    id: 'ember',
    name: 'settings.accents.ember',
    dark: { color: '#E8833A', text: '#1B1B1D' },
    light: { color: '#C2621F', text: '#FFFFFF' },
  },
  {
    id: 'glacier',
    name: 'settings.accents.glacier',
    dark: { color: '#6FA8DC', text: '#1B1B1D' },
    light: { color: '#2F72B8', text: '#FFFFFF' },
  },
  {
    id: 'mint',
    name: 'settings.accents.mint',
    dark: { color: '#4FC79A', text: '#1B1B1D' },
    light: { color: '#1E8C63', text: '#FFFFFF' },
  },
  {
    id: 'violet',
    name: 'settings.accents.violet',
    dark: { color: '#A78BFA', text: '#1B1B1D' },
    light: { color: '#6D4FD8', text: '#FFFFFF' },
  },
  {
    id: 'mono',
    name: 'settings.accents.mono',
    dark: { color: '#D9D9D9', text: '#1B1B1D' },
    light: { color: '#2A2A2D', text: '#FFFFFF' },
  },
];

export const DEFAULT_ACCENT_ID = 'ember';

export const getAccent = (id?: string | null): AccentProps =>
  ACCENTS.find((a) => a.id === id) || (ACCENTS.find((a) => a.id === DEFAULT_ACCENT_ID) as AccentProps);

/** Every CSS variable for a theme and accent pair. */
export const themeCssVariables = (themeId?: string | null, accentId?: string | null): Record<string, string> => {
  const theme = getTheme(themeId);
  const accent = getAccent(accentId)[theme.isLight ? 'light' : 'dark'];
  return {
    ...theme.cssVariables,
    '--app-accent': accent.color,
    '--app-hover-color': accent.color,
    '--app-button-text': accent.text,
  };
};
