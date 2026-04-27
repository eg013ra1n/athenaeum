import React from 'react';
import type { ScanRootWithAvailability } from '../../../types/models';

interface Props {
  rootId: number | null;
  scanRoots: ScanRootWithAvailability[];
  onChange: (rootId: number | null) => void;
  disabled?: boolean;
}

export const MasterRootConfig: React.FC<Props> = ({
  rootId,
  scanRoots,
  onChange,
  disabled,
}) => {
  return (
    <select
      value={rootId ?? ''}
      onChange={(e) =>
        onChange(e.target.value === '' ? null : Number(e.target.value))
      }
      disabled={disabled}
      className="h-7 bg-surface-hover border border-border rounded-lg px-2 text-xs focus:outline-none focus:ring-2 focus:ring-accent max-w-[420px] disabled:opacity-50 disabled:cursor-not-allowed"
    >
      <option value="">— None (rule abstains) —</option>
      {scanRoots.map((root) => (
        <option key={root.id!} value={root.id!}>
          {root.path}
        </option>
      ))}
    </select>
  );
};
