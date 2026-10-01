import React, { ReactNode } from 'react';
import { ChevronRight } from 'lucide-react';
import clsx from 'clsx';

/**
 * One group of adjustments in Advanced mode: a header that opens and
 * closes it, with room on the right for the section's own buttons (reorder
 * handle, hide), and its controls below. Sections are separated by a thin
 * rule rather than boxed, so a long panel stays calm to scan.
 */
export default function AdjustmentSection({
  title,
  open,
  onToggle,
  modified = false,
  actions,
  children,
  sectionRef,
  style,
  highlight = false,
}: {
  /** For drag-and-drop: the element that moves, and how it is styled. */
  sectionRef?: (el: HTMLElement | null) => void;
  style?: React.CSSProperties;
  /** A drop target line above the section while another is dragged over it. */
  highlight?: boolean;
  title: string;
  open: boolean;
  onToggle: () => void;
  /** A dot beside the title when anything in the section is changed. */
  modified?: boolean;
  /** Small buttons shown at the right of the header. */
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section
      ref={sectionRef}
      style={style}
      className={clsx(
        'border-b border-surface last:border-b-0 bg-bg-secondary',
        highlight && 'shadow-[inset_0_2px_0_0_var(--color-accent)]',
      )}
    >
      <div className="group flex items-center gap-1">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="flex flex-1 items-center gap-2 py-2.5 text-left focus-visible:outline-2 focus-visible:outline-accent rounded-sm"
        >
          <ChevronRight
            size={14}
            className={clsx('shrink-0 text-text-secondary transition-transform duration-200', open && 'rotate-90')}
          />
          <span className="text-[13px] font-semibold tracking-wide text-text-primary">{title}</span>
          {modified && <span className="h-1.5 w-1.5 rounded-full bg-accent" aria-hidden />}
        </button>
        {actions && <div className="flex items-center gap-0.5">{actions}</div>}
      </div>
      {open && <div className="flex flex-col gap-1 pb-4 pl-1">{children}</div>}
    </section>
  );
}

/** A small heading inside Basic mode's airy groups. */
export function BasicGroupTitle({ children }: { children: ReactNode }) {
  return <h3 className="mb-1 text-[11px] font-semibold uppercase tracking-[0.08em] text-text-secondary">{children}</h3>;
}
