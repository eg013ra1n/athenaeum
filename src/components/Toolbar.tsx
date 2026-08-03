import type { LucideIcon } from 'lucide-react';
import type { ReactNode } from 'react';

// ---------------------------------------------------------------------------
// ToolbarContainer
// ---------------------------------------------------------------------------

interface ToolbarContainerProps {
  children: ReactNode;
}

export function ToolbarContainer({ children }: ToolbarContainerProps) {
  return (
    <div className="flex items-start gap-2 flex-wrap flex-shrink-0">
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// ToolbarButton
// ---------------------------------------------------------------------------

interface ToolbarButtonProps {
  variant?: 'primary' | 'default' | 'danger';
  icon?: LucideIcon;
  disabled?: boolean;
  onClick?: () => void;
  title?: string;
  children?: ReactNode;
  type?: 'button' | 'submit' | 'reset';
}

export function ToolbarButton({
  variant = 'default',
  icon: Icon,
  disabled,
  onClick,
  title,
  children,
  type = 'button',
}: ToolbarButtonProps) {
  const iconOnly = Icon && !children;

  const variantClasses: Record<NonNullable<ToolbarButtonProps['variant']>, string> = {
    primary:
      'bg-accent hover:bg-accent-hover text-white disabled:bg-surface-hover disabled:cursor-not-allowed',
    default:
      'bg-surface-hover border border-border text-content-secondary hover:bg-surface-elevated disabled:opacity-30 disabled:cursor-default',
    danger:
      'bg-error hover:brightness-90 text-white disabled:opacity-30 disabled:cursor-default',
  };

  return (
    <button
      type={type}
      disabled={disabled}
      onClick={onClick}
      title={title}
      className={[
        'h-7 inline-flex items-center justify-center gap-1.5 text-xs font-medium rounded-lg transition-colors',
        iconOnly ? 'w-10' : 'px-3',
        variantClasses[variant ?? 'default'],
      ].join(' ')}
    >
      {Icon && <Icon size={12} />}
      {children}
    </button>
  );
}

// ---------------------------------------------------------------------------
// ToolbarDivider
// ---------------------------------------------------------------------------

export function ToolbarDivider() {
  return <span className="text-border h-7 flex items-center">|</span>;
}

// ---------------------------------------------------------------------------
// ToolbarInfo — non-interactive status chip (info tone), toolbar height
// ---------------------------------------------------------------------------

interface ToolbarInfoProps {
  icon?: LucideIcon;
  children: ReactNode;
}

export function ToolbarInfo({ icon: Icon, children }: ToolbarInfoProps) {
  return (
    <div className="h-7 inline-flex items-center gap-1.5 px-3 text-xs font-medium rounded-lg bg-info-muted border border-info/50 text-info">
      {Icon && <Icon size={12} />}
      {children}
    </div>
  );
}
