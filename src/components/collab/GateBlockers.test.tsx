import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import GateBlockers from './GateBlockers';
import type { GateReport } from '../../types/models';

afterEach(cleanup);

const gate: GateReport = {
  projectId: 'p', total: 10, publishable: 1, rows: [
    { frameId: 1, filename: 'a.fits', fwhmArcsec: null, eccentricity: null, starsDetected: null, trailed: null, publishable: false, failures: ['no analysis'], rules: [] },
    { frameId: 2, filename: 'b.fits', fwhmArcsec: null, eccentricity: null, starsDetected: null, trailed: null, publishable: false, failures: ['no coordinates'], rules: [] },
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

  it('pluralises singular counts (1 frame has…, 1 filter name needs a mapping)', () => {
    const g: GateReport = {
      projectId: 'p', total: 1, publishable: 0, rows: [],
      blockers: [
        { kind: 'analyze', frames: 1, sets: [10], names: [] },
        { kind: 'mapFilter', frames: 1, sets: [10], names: [{ instrume: 'ATR2600M', filterRaw: '', frames: 1 }] },
        { kind: 'outsideTarget', frames: 1, sets: [10], names: [] },
      ],
    };
    render(<GateBlockers gate={g} solveBusy={false} analyzeBusy={new Set()} onMapFilters={vi.fn()} onOpenCalibration={vi.fn()} onSolve={vi.fn()} onAnalyze={vi.fn()} />);
    expect(screen.getByText('1 frame has no analysis')).toBeInTheDocument();
    expect(screen.getByText('1 filter name needs a mapping')).toBeInTheDocument();
    expect(screen.getByText('1 frame is outside the target')).toBeInTheDocument();
  });

  it('disables Analyze while its own set is busy, a menu\'s per-set item independently', () => {
    const g: GateReport = {
      projectId: 'p', total: 2, publishable: 0, rows: [],
      blockers: [{ kind: 'analyze', frames: 2, sets: [10, 11], names: [] }],
    };
    render(<GateBlockers gate={g} solveBusy={false} analyzeBusy={new Set([11])} onMapFilters={vi.fn()} onOpenCalibration={vi.fn()} onSolve={vi.fn()} onAnalyze={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: /Analyze/ }));
    const busyItem = screen.getByRole('menuitem', { name: /Set #11/ });
    const freeItem = screen.getByRole('menuitem', { name: 'Set #10' });
    expect(busyItem).toBeDisabled();
    expect(freeItem).not.toBeDisabled();
  });

  it('menu items are real buttons; Escape and an outside click both close the menu', () => {
    const onOpenCalibration = vi.fn();
    const g: GateReport = {
      projectId: 'p', total: 2, publishable: 0, rows: [],
      blockers: [{ kind: 'linkCalibration', frames: 2, sets: [10, 11], names: [] }],
    };
    render(
      <div>
        <button type="button">outside</button>
        <GateBlockers gate={g} solveBusy={false} analyzeBusy={new Set()} onMapFilters={vi.fn()} onOpenCalibration={onOpenCalibration} onSolve={vi.fn()} onAnalyze={vi.fn()} />
      </div>,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Open calibration' }));
    const item = screen.getByRole('menuitem', { name: /Set #10/ });
    expect(item.tagName).toBe('BUTTON');
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('menu')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Open calibration' }));
    expect(screen.getByRole('menu')).toBeInTheDocument();
    fireEvent.mouseDown(screen.getByRole('button', { name: 'outside' }));
    expect(screen.queryByRole('menu')).toBeNull();
    expect(onOpenCalibration).not.toHaveBeenCalled();
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
