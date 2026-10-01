import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import ReasonGroupAction, { type ReasonGroupActionProps } from './ReasonGroupAction';
import { SidePanel } from '../../ui';
import { fromOwn } from './frames';
import type { OwnFrameRow } from '../../../types/models';

afterEach(cleanup);

function own(o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1,
    frameUuid: null,
    fileName: 'f1.fits',
    setId: 1,
    setName: 'M31',
    night: null,
    filter: 'Ha',
    filterMapped: true,
    camera: 'ASI2600MM Pro',
    exptimeSec: 300,
    byteSize: 1_000_000,
    fwhmArcsec: null,
    eccentricity: null,
    starsDetected: null,
    medianSnr: null,
    segment: 'held',
    contributorState: 'held',
    contributorReason: null,
    failures: [],
    contentVersion: null,
    pubState: null,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    localState: null,
    publishedAt: null,
    lastError: null,
    rules: [],
    path: null,
    accepted: null,
    ...o,
  };
}

function baseProps(overrides: Partial<ReasonGroupActionProps> = {}): ReasonGroupActionProps {
  return {
    kind: '',
    rows: [fromOwn(own())],
    solveBusy: false,
    analyzeBusy: new Set<number>(),
    onSolve: vi.fn(),
    onAnalyze: vi.fn(),
    onOpenCalibration: vi.fn(),
    onMapFilters: vi.fn(),
    ...overrides,
  };
}

describe('ReasonGroupAction', () => {
  it('the threshold kind renders the quality text and no button', () => {
    render(<ReasonGroupAction {...baseProps({ kind: 'threshold' })} />);
    expect(screen.getByText('quality — the frames themselves')).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('mapFilter calls onMapFilters', () => {
    const onMapFilters = vi.fn();
    render(<ReasonGroupAction {...baseProps({ kind: 'mapFilter', onMapFilters })} />);
    fireEvent.click(screen.getByRole('button', { name: 'Map filters' }));
    expect(onMapFilters).toHaveBeenCalledTimes(1);
  });

  it('attest offers "Attest as calibrated…" which calls onOpenCalibration(setId)', () => {
    const onOpenCalibration = vi.fn();
    render(
      <ReasonGroupAction
        {...baseProps({
          kind: 'attest',
          rows: [fromOwn(own({ setId: 7, setName: 'M31' }))],
          onOpenCalibration,
        })}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Attest as calibrated…' }));
    expect(onOpenCalibration).toHaveBeenCalledWith(7);
  });

  it('also offers "Open calibration" alongside the attest action, same target', () => {
    const onOpenCalibration = vi.fn();
    render(
      <ReasonGroupAction
        {...baseProps({
          kind: 'linkCalibration',
          rows: [fromOwn(own({ setId: 9, setName: 'M42' }))],
          onOpenCalibration,
        })}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Open calibration' }));
    expect(onOpenCalibration).toHaveBeenCalledWith(9);
  });

  it('uuid and outsideTarget render their muted texts with no button', () => {
    const { rerender } = render(<ReasonGroupAction {...baseProps({ kind: 'uuid' })} />);
    expect(screen.getByText('re-scan the folder')).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();

    rerender(<ReasonGroupAction {...baseProps({ kind: 'outsideTarget' })} />);
    expect(screen.getByText('outside the target radius')).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('an empty kind renders nothing', () => {
    const { container } = render(<ReasonGroupAction {...baseProps({ kind: '' })} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('solve renders "Solve N" (N = the group\'s frames) and calls onSolve with their ids', () => {
    const onSolve = vi.fn();
    render(
      <ReasonGroupAction
        {...baseProps({
          kind: 'solve',
          rows: [fromOwn(own({ frameId: 4 })), fromOwn(own({ frameId: 5 }))],
          onSolve,
        })}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Solve 2' }));
    expect(onSolve).toHaveBeenCalledWith([4, 5]);
  });

  it('solve is disabled while a solve is already running', () => {
    render(<ReasonGroupAction {...baseProps({ kind: 'solve', solveBusy: true })} />);
    expect(screen.getByRole('button', { name: 'Solve 1' })).toBeDisabled();
  });

  it('analyze with a single set is a plain button that calls onAnalyze(setId)', () => {
    const onAnalyze = vi.fn();
    render(
      <ReasonGroupAction
        {...baseProps({ kind: 'analyze', rows: [fromOwn(own({ setId: 3, setName: 'M31' }))], onAnalyze })}
      />,
    );
    expect(screen.queryByRole('menu')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Analyze' }));
    expect(onAnalyze).toHaveBeenCalledWith(3);
  });

  it('analyze with two sets opens a menu naming both by setName; a busy set is disabled', () => {
    const onAnalyze = vi.fn();
    render(
      <ReasonGroupAction
        {...baseProps({
          kind: 'analyze',
          rows: [fromOwn(own({ setId: 10, setName: 'M31' })), fromOwn(own({ setId: 11, setName: 'M42' }))],
          analyzeBusy: new Set([11]),
          onAnalyze,
        })}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Analyze' }));
    const busyItem = screen.getByRole('menuitem', { name: /M42/ });
    const freeItem = screen.getByRole('menuitem', { name: 'M31' });
    expect(busyItem).toBeDisabled();
    expect(freeItem).not.toBeDisabled();
    fireEvent.click(freeItem);
    expect(onAnalyze).toHaveBeenCalledWith(10);
  });

  it('Escape on an open set menu over a docked side panel closes the menu alone (overlay stack)', () => {
    function Harness() {
      const [docked, setDocked] = useState(true);
      return (
        <>
          {docked && <SidePanel title="t" label="Frame details" onClose={() => setDocked(false)}>kv</SidePanel>}
          <ReasonGroupAction
            {...baseProps({
              kind: 'analyze',
              rows: [fromOwn(own({ setId: 10, setName: 'M31' })), fromOwn(own({ setId: 11, setName: 'M42' }))],
            })}
          />
        </>
      );
    }
    render(<Harness />);
    fireEvent.click(screen.getByRole('button', { name: 'Analyze' }));
    expect(screen.getByRole('menu')).toBeInTheDocument();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('menu')).toBeNull();
    expect(screen.getByRole('complementary', { name: 'Frame details' })).toBeInTheDocument();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('complementary', { name: 'Frame details' })).toBeNull();
  });

  it('an outside mousedown still closes the open menu', () => {
    render(
      <div>
        <span>outside</span>
        <ReasonGroupAction
          {...baseProps({
            kind: 'analyze',
            rows: [fromOwn(own({ setId: 10, setName: 'M31' })), fromOwn(own({ setId: 11, setName: 'M42' }))],
          })}
        />
      </div>,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Analyze' }));
    fireEvent.mouseDown(screen.getByText('outside'));
    expect(screen.queryByRole('menu')).toBeNull();
  });
});
