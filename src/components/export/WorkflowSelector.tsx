import type { SirilWorkflow } from '../../types/export';

interface WorkflowSelectorProps {
  value: SirilWorkflow;
  onChange: (workflow: SirilWorkflow) => void;
}

const WORKFLOWS: { workflow: SirilWorkflow; label: string; description: string }[] = [
  {
    workflow: 'mono_preprocessing',
    label: 'Mono preprocessing (per filter)',
    description: 'Process each filter separately - best for narrowband or LRGB with mono camera',
  },
  {
    workflow: 'osc_preprocessing',
    label: 'OSC (one-shot color)',
    description: 'Process color camera images with debayering',
  },
  {
    workflow: 'lrgb_processing',
    label: 'LRGB combination',
    description: 'Combine L, R, G, B channels into color image',
  },
];

export function WorkflowSelector({ value, onChange }: WorkflowSelectorProps) {
  return (
    <div className="space-y-2">
      {WORKFLOWS.map(({ workflow, label, description }) => (
        <button
          key={workflow}
          onClick={() => onChange(workflow)}
          className={`w-full text-left p-3 rounded-lg border transition-colors ${
            value === workflow
              ? 'bg-accent/20 border-accent'
              : 'bg-surface-hover/50 border-border hover:bg-surface-hover hover:border-content-muted'
          }`}
        >
          <div className="flex items-center justify-between">
            <div>
              <div className="font-medium">{label}</div>
              <div className="text-sm text-content-muted mt-0.5">{description}</div>
            </div>
            {value === workflow && <div className="w-4 h-4 rounded-full bg-accent" />}
          </div>
        </button>
      ))}
    </div>
  );
}
