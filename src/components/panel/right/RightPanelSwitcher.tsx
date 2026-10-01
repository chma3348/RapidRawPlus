import { motion } from 'framer-motion';
import {
  SlidersHorizontal,
  Info,
  Crop,
  Layers,
  Paintbrush,
  SwatchBook,
  FileInput,
  Cloud,
  type LucideIcon,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Panel } from '../../ui/AppProperties';

interface PanelOptions {
  icon: LucideIcon;
  id: Panel;
  title: string;
  /** The short name shown under the icon when rail labels are on. */
  label: string;
  fallback: string;
}

interface RightPanelSwitcherProps {
  activePanel: Panel | null;
  onPanelSelect(id: Panel): void;
  isInstantTransition: boolean;
  layout?: 'horizontal' | 'vertical';
  /** Show each panel's name under its icon (Layout menu). */
  showLabels?: boolean;
}

const panelGroups: Array<Array<PanelOptions>> = [
  [{ id: Panel.Metadata, icon: Info, title: 'editor.switcher.tooltips.info', label: 'info', fallback: 'Info' }],
  [
    {
      id: Panel.Adjustments,
      icon: SlidersHorizontal,
      title: 'editor.switcher.tooltips.adjust',
      label: 'adjust',
      fallback: 'Adjust',
    },
    { id: Panel.Crop, icon: Crop, title: 'editor.switcher.tooltips.crop', label: 'crop', fallback: 'Crop' },
    { id: Panel.Masks, icon: Layers, title: 'editor.switcher.tooltips.masks', label: 'masks', fallback: 'Masks' },
    {
      id: Panel.Ai,
      icon: Paintbrush,
      title: 'editor.switcher.tooltips.inpaint',
      label: 'retouch',
      fallback: 'Retouch',
    },
    { id: Panel.Sky, icon: Cloud, title: 'editor.switcher.tooltips.sky', label: 'sky', fallback: 'Sky' },
  ],
  [
    {
      id: Panel.Presets,
      icon: SwatchBook,
      title: 'editor.switcher.tooltips.presets',
      label: 'presets',
      fallback: 'Presets',
    },
    {
      id: Panel.Export,
      icon: FileInput,
      title: 'editor.switcher.tooltips.export',
      label: 'export',
      fallback: 'Export',
    },
  ],
];

export default function RightPanelSwitcher({
  activePanel,
  onPanelSelect,
  isInstantTransition,
  layout = 'vertical',
  showLabels = false,
}: RightPanelSwitcherProps) {
  const { t } = useTranslation();
  const isHorizontal = layout === 'horizontal';

  return (
    <div className={isHorizontal ? 'flex items-center overflow-x-auto p-1 gap-1' : 'flex flex-col p-1 gap-1 h-full'}>
      {panelGroups.map((group, groupIndex) => (
        <div key={groupIndex} className={isHorizontal ? 'flex items-center gap-1' : 'flex flex-col gap-1'}>
          {groupIndex > 0 && (
            <div
              className={isHorizontal ? 'w-px h-6 bg-surface self-stretch my-auto' : 'w-6 h-px bg-surface self-center'}
            />
          )}
          {group.map(({ id, icon: Icon, title, label, fallback }) => (
            <button
              className={`relative rounded-md transition-colors duration-200 ${isHorizontal ? 'p-2 shrink-0' : 'p-2'} ${
                showLabels ? 'flex flex-col items-center gap-0.5 min-w-[52px]' : ''
              } ${
                activePanel === id
                  ? 'text-text-primary'
                  : 'text-text-secondary hover:bg-surface hover:text-text-primary'
              }`}
              key={id}
              onClick={() => onPanelSelect(id)}
              data-tooltip={t(title)}
            >
              {activePanel === id && (
                <motion.div
                  layoutId="active-panel-indicator"
                  className="absolute inset-0 bg-surface rounded-md"
                  transition={isInstantTransition ? { duration: 0 } : { type: 'spring', bounce: 0.2, duration: 0.4 }}
                />
              )}
              <Icon size={20} className="relative z-10" />
              {showLabels && (
                <span className="relative z-10 text-[10px] leading-none">
                  {t(`editor.switcher.labels.${label}`, { defaultValue: fallback })}
                </span>
              )}
            </button>
          ))}
        </div>
      ))}
    </div>
  );
}
