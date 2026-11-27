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

describe('PlateSolveBatchPanel, failed frames', () => {
  const renderWithFailure = (code: string) => {
    ctx.current = {
      enqueuePlateSolve: vi.fn(() => 1),
      cancelBatch: vi.fn(),
      dismissCompleted: vi.fn(),
      cancelForeignRun: vi.fn(),
      foreignRun: null,
      activeBatches: new Map([
        [
          1,
          {
            isComplete: true,
            isCancelling: false,
            progress: null,
            currentFilename: null,
            errorMessage: null,
            summary: { solved: 0, failed: 1, total: 1, total_time_ms: 1000 },
            frameStatuses: new Map([
              [7, { kind: 'failed', error: 'refused', code, filename: 'a.fits' }],
            ]),
          },
        ],
      ]),
    };
    const view = render(<PlateSolveBatchPanel frameIds={[7]} />);
    fireEvent.click(screen.getByRole('button', { name: /Plate Solve All/ }));
    return view;
  };

  const rowIconClass = (container: HTMLElement) =>
    container.querySelector('li svg')?.getAttribute('class') ?? '';

  it('a NOT_CONFIDENT failure is styled as a warning', () => {
    const { container } = renderWithFailure('NOT_CONFIDENT');
    expect(rowIconClass(container)).toContain('text-warning');
  });

  it('a TOO_FEW_MATCHED failure is styled as a warning', () => {
    const { container } = renderWithFailure('TOO_FEW_MATCHED');
    expect(rowIconClass(container)).toContain('text-warning');
  });

  it('a REJECTED_LOW_CONFIDENCE failure is styled as a warning', () => {
    const { container } = renderWithFailure('REJECTED_LOW_CONFIDENCE');
    expect(rowIconClass(container)).toContain('text-warning');
  });

  it('a VERIFY_GAP failure stays an error', () => {
    const { container } = renderWithFailure('VERIFY_GAP');
    expect(rowIconClass(container)).toContain('text-error');
  });
});
