interface SwitchRowProps {
  title: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}

export function SwitchRow({ title, description, checked, disabled, onChange }: SwitchRowProps) {
  return (
    <label className={`flex items-start gap-3 px-2 py-2 rounded-lg transition hover:bg-surface-hover/40 ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-1 w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
      />
      <span className="flex-1 min-w-0">
        <span className="block text-sm font-medium text-content">{title}</span>
        <span className="block text-xs text-content-muted leading-relaxed">{description}</span>
      </span>
    </label>
  );
}
