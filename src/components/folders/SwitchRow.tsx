import { Checkbox } from '../settings/Checkbox';

interface SwitchRowProps {
  title: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}

/**
 * A thin wrapper over the house `Checkbox` (settings redesign spec
 * 2026-09-18 §4) so there is one checkbox control in the codebase — props
 * unchanged from before. Keeps its own hover-tinted, padded row around it,
 * since `Checkbox` itself renders a bare label with no such chrome.
 */
export function SwitchRow({ title, description, checked, disabled, onChange }: SwitchRowProps) {
  return (
    <div className="rounded-lg transition hover:bg-surface-hover/40 px-2 py-2">
      <Checkbox
        role="switch"
        checked={checked}
        disabled={disabled}
        onChange={onChange}
        label={<span className="font-medium text-content">{title}</span>}
        description={description}
      />
    </div>
  );
}
