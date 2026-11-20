import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { PlateSolveBatchPanel } from './PlateSolveBatchPanel';

const { ctx } = vi.hoisted(() => ({ ctx: { current: {} as Record<string, unknown> } }));
vi.mock('../../contexts/PlateSolveProgressContext', () => ({
  usePlateSolveProgressContext: () => ctx.current,
}));

afterEach(() => cleanup());

describe('PlateSolveBatchPanel, a run this page did not start', () => {
  it('assistant_run_shows_progress_with_cancel', () => {
    const cancelForeignRun = vi.fn();
    ctx.current = {
      enqueuePlateSolve: vi.fn(),
      cancelBatch: vi.fn(),
      dismissCompleted: vi.fn(),
      activeBatches: new Map(),
      cancelForeignRun,
      foreignRun: { runId: 'solve-a', origin: 'assistant', done: 3, total: 10, currentFile: 'M31-0004.fits', isCancelling: false },
    };
    render(<PlateSolveBatchPanel hideTriggerButtons />);
    expect(screen.getByText('Plate solve started from the assistant — 3 of 10, M31-0004.fits')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancelForeignRun).toHaveBeenCalledTimes(1);
  });

  it('a_ui_run_this_page_did_not_start_shows_its_own_words_with_cancel', () => {
    const cancelForeignRun = vi.fn();
    ctx.current = {
      enqueuePlateSolve: vi.fn(),
      cancelBatch: vi.fn(),
      dismissCompleted: vi.fn(),
      activeBatches: new Map(),
      cancelForeignRun,
      foreignRun: { runId: 'solve-u', origin: 'ui', done: 2, total: 5, currentFile: 'x.fits', isCancelling: false },
    };
    render(<PlateSolveBatchPanel hideTriggerButtons />);
    expect(screen.getByText('A plate solve batch is running — 2 of 5, x.fits')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancelForeignRun).toHaveBeenCalledTimes(1);
  });

  it('shows_nothing_without_an_assistant_run', () => {
    ctx.current = {
      enqueuePlateSolve: vi.fn(),
      cancelBatch: vi.fn(),
      dismissCompleted: vi.fn(),
      activeBatches: new Map(),
      cancelForeignRun: vi.fn(),
      foreignRun: null,
    };
    render(<PlateSolveBatchPanel hideTriggerButtons />);
    expect(screen.queryByText(/started from the assistant/)).toBeNull();
  });
});
