import clsx from 'clsx';

interface ButtonProps {
  autoFocus?: boolean;
  children: any;
  className?: string;
  disabled?: boolean;
  onClick: any;
  size?: 'sm' | 'md' | string;
  title?: string;
  tabIndex?: number;
  /** primary (accent, the default), secondary (quiet), ghost (no fill), destructive (red). */
  variant?: 'primary' | 'secondary' | 'ghost' | 'destructive' | string;
}

const Button = ({
  children,
  onClick,
  disabled,
  className = '',
  variant = 'primary',
  size = 'md',
  ...props
}: ButtonProps) => {
  // A caller's own background or text colour wins over the variant's.
  const ownBg = /(^|\s)bg-/.test(className);
  const ownText = /(^|\s)text-(?!xs|sm|base|md|lg|xl|\d|left|right|center)/.test(className);

  const look: Record<string, { bg: string; text: string }> = {
    primary: { bg: 'bg-accent shadow-shiny', text: 'text-button-text' },
    secondary: { bg: 'bg-surface hover:bg-card-active', text: 'text-text-primary' },
    ghost: { bg: 'hover:bg-surface', text: 'text-text-primary' },
    destructive: { bg: 'bg-red-600 hover:bg-red-500', text: 'text-white' },
  };
  const chosen = look[variant] ?? look.primary;

  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={clsx(
        'flex items-center justify-center gap-2 font-semibold rounded-md',
        size === 'sm' ? 'py-1 px-3 text-sm' : 'py-2 px-4 text-base',
        'transition-transform duration-200 hover:scale-[1.01] active:scale-[.98]',
        'disabled:opacity-50 disabled:cursor-not-allowed disabled:shadow-none disabled:hover:scale-100',
        !ownBg && chosen.bg,
        !ownText && chosen.text,
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
};

export default Button;
