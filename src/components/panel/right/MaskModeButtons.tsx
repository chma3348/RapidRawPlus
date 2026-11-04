import type { MouseEvent } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import { Minus, Plus, SquaresIntersect } from 'lucide-react';
import { SubMaskMode } from './Masks';

interface Props {
  /** Shown while the mask (or one of its parts) is selected. */
  visible: boolean;
  /** Subtract and Intersect need something to work on. */
  hasParts: boolean;
  /** Opens the tool menu for adding a part in `mode`. */
  onPick(event: MouseEvent, mode: SubMaskMode): void;
  /** Tooltips; the Remove panel says "edit" where the Masks panel says "mask". */
  hints: Record<SubMaskMode, string>;
}

/**
 * A set mask's Add / Subtract / Intersect buttons, as in Lightroom: each
 * opens every masking tool, the Brush first, and adds that tool's selection
 * to the mask in its mode. Shared by the Masks and Remove panels.
 */
export default function MaskModeButtons({ visible, hasParts, onPick, hints }: Props) {
  const { t } = useTranslation();
  const buttons = [
    { mode: SubMaskMode.Additive, icon: Plus, label: t('editor.masks.actions.add', { defaultValue: 'Add' }) },
    {
      mode: SubMaskMode.Subtractive,
      icon: Minus,
      label: t('editor.masks.actions.subtract', { defaultValue: 'Subtract' }),
    },
    {
      mode: SubMaskMode.Intersect,
      icon: SquaresIntersect,
      label: t('editor.masks.actions.intersect', { defaultValue: 'Intersect' }),
    },
  ].filter((b) => b.mode === SubMaskMode.Additive || hasParts);

  return (
    <AnimatePresence initial={false}>
      {visible && (
        <motion.div
          key="mask-mode-buttons"
          layout="position"
          initial={{ opacity: 0, height: 0, overflow: 'hidden' }}
          animate={{ opacity: 1, height: 'auto', overflow: 'hidden' }}
          exit={{ opacity: 0, height: 0, overflow: 'hidden' }}
          transition={{ duration: 0.2 }}
          className="flex items-center gap-1 pl-2 ml-3.75 py-1"
        >
          {buttons.map(({ mode, icon: Icon, label }) => (
            <button
              key={mode}
              type="button"
              data-tooltip={hints[mode]}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onPick(e, mode);
              }}
              className="flex flex-1 items-center justify-center gap-1.5 rounded-md px-2 py-1 text-xs bg-bg-secondary hover:bg-card-active text-text-secondary hover:text-text-primary transition-colors"
            >
              <Icon size={14} />
              {label}
            </button>
          ))}
        </motion.div>
      )}
    </AnimatePresence>
  );
}
