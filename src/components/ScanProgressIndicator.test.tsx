import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ScanProgressIndicator } from './ScanProgressIndicator';

const scan = vi.hoisted(() => ({ phase: 'zips' }));

vi.mock('../contexts/ScanProgressContext', () => ({
  useScanProgressContext: () => ({
    activeScans: new Map([
      [
        1,
        {
          rootId: 1,
          rootPath: '/data/astro',
          isComplete: false,
          result: null,
          progress: { current: 2, total: 5, current_file: 'M31_night2.zip', percent: 40, root_id: 1, phase: scan.phase },
        },
      ],
    ]),
    cancelScan: vi.fn(),
  }),
}));

describe('ScanProgressIndicator phases', () => {
  it('names the zip phase and shows the zip being read', () => {
    scan.phase = 'zips';
    render(<ScanProgressIndicator />);
    expect(screen.getByRole('heading', { name: 'Reading zip archives' })).toBeInTheDocument();
    expect(screen.getByText('M31_night2.zip')).toBeInTheDocument();
  });

  it('keeps the existing labels', () => {
    scan.phase = 'caching';
    render(<ScanProgressIndicator />);
    expect(screen.getByRole('heading', { name: 'Building duplicate cache...' })).toBeInTheDocument();
  });
});
