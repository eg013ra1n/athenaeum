import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import GateBlockers from './GateBlockers';
import type { GateReport } from '../../types/models';

afterEach(cleanup);

const gate: GateReport = {
  projectId: 'p', total: 10, publishable: 1, rows: [
    { frameId: 1, filename: 'a.fits', fwhmArcsec: null, eccentricity: null, starsDetected: null, trailed: null, publishable: false, failures: ['no analysis'] },
    { frameId: 2, filename: 'b.fits', fwhmArcsec: null, eccentricity: null, starsDetected: null, trailed: null, publishable: false, failures: ['no coordinates'] },
  ],
  blockers: [
    { kind: 'analyze', frames: 79, sets: [10], names: [] },
    { kind: 'solve', frames: 12, sets: [10], names: [] },
    { kind: 'linkCalibration', frames: 891, sets: [10, 11], names: [] },
    { kind: 'attest', frames: 891, sets: [10, 11], names: [] },
    { kind: 'mapFilter', frames: 903, sets: [10], names: [{ instrume: 'ATR2600M', filterRaw: '', frames: 891 }, { instrume: 'QHY268M', filterRaw: 'Slot 0', frames: 12 }] },
    { kind: 'threshold', frames: 4, sets: [10], names: [] },
    { kind: 'outsideTarget', frames: 3, sets: [10], names: [] },
  ],
};

describe('GateBlockers', () => {
  it('renders one line per cause with its count and the right button', () => {
    const onMapFilters = vi.fn(); const onOpenCalibration = vi.fn(); const onSolve = vi.fn(); const onAnalyze = vi.fn();
    render(<GateBlockers gate={gate} solveBusy={false} analyzeBusy={new Set()} onMapFilters={onMapFilters} onOpenCalibration={onOpenCalibration} onSolve={onSolve} onAnalyze={onAnalyze} />);
    expect(screen.getByText('79 frames have no analysis')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Analyze' }));
    expect(onAnalyze).toHaveBeenCalledWith(10);
    expect(screen.getByText('12 frames have no coordinates or pixel scale')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Solve 12 frames' }));
    expect(onSolve).toHaveBeenCalledWith([2]);
    expect(screen.getByText('891 frames are not calibrated')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Open calibration' }));
    // Two sets: a menu of set ids appears; picking one calls back with it.
    fireEvent.click(screen.getByRole('menuitem', { name: /Set #11/ }));
    expect(onOpenCalibration).toHaveBeenCalledWith(11);
    expect(screen.getByRole('button', { name: 'Attest as calibrated…' })).toBeInTheDocument();
    expect(screen.getByText('2 filter names need a mapping')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Map filters' }));
    expect(onMapFilters).toHaveBeenCalled();
    expect(screen.getByText('4 frames fail a threshold')).toBeInTheDocument();
    expect(screen.getByText('3 frames are outside the target')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /threshold/ })).toBeNull();
  });

  it('renders nothing without blockers', () => {
    const { container } = render(<GateBlockers gate={{ ...gate, blockers: [] }} solveBusy={false} analyzeBusy={new Set()} onMapFilters={vi.fn()} onOpenCalibration={vi.fn()} onSolve={vi.fn()} onAnalyze={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders the calibration line once from buildMasters alone, with no linkCalibration blocker', () => {
    const onOpenCalibration = vi.fn();
    const g: GateReport = {
      projectId: 'p', total: 5, publishable: 0, rows: [],
      blockers: [{ kind: 'buildMasters', frames: 5, sets: [20], names: [] }],
    };
    render(<GateBlockers gate={g} solveBusy={false} analyzeBusy={new Set()} onMapFilters={vi.fn()} onOpenCalibration={onOpenCalibration} onSolve={vi.fn()} onAnalyze={vi.fn()} />);
    expect(screen.getAllByText('5 frames are not calibrated')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Open calibration' }));
    expect(onOpenCalibration).toHaveBeenCalledWith(20);
    expect(screen.getByRole('button', { name: 'Attest as calibrated…' })).toBeInTheDocument();
  });
});
