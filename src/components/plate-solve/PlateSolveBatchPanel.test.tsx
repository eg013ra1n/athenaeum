import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { PlateSolveBatchPanel } from './PlateSolveBatchPanel';

const { ctx } = vi.hoisted(() => ({ ctx: { current: {} as Record<string, unknown> } }));
vi.mock('../../contexts/PlateSolveProgressContext', () => ({
  usePlateSolveProgressContext: () => ctx.current,
}));

afterEach(() => cleanup());

describe('PlateSolveBatchPanel, a run the assistant started', () => {
  it('assistant_run_shows_progress_with_cancel', () => {
    const cancelAssistantRun = vi.fn();
    ctx.current = {
      enqueuePlateSolve: vi.fn(),
      cancelBatch: vi.fn(),
      dismissCompleted: vi.fn(),
      activeBatches: new Map(),
      cancelAssistantRun,
      assistantRun: { runId: 'solve-a', done: 3, total: 10, currentFile: 'M31-0004.fits', isCancelling: false },
    };
    render(<PlateSolveBatchPanel hideTriggerButtons />);
    expect(screen.getByText('Plate solve started from the assistant — 3 of 10, M31-0004.fits')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancelAssistantRun).toHaveBeenCalledTimes(1);
  });

  it('shows_nothing_without_an_assistant_run', () => {
    ctx.current = {
      enqueuePlateSolve: vi.fn(),
      cancelBatch: vi.fn(),
      dismissCompleted: vi.fn(),
      activeBatches: new Map(),
      cancelAssistantRun: vi.fn(),
      assistantRun: null,
    };
    render(<PlateSolveBatchPanel hideTriggerButtons />);
    expect(screen.queryByText(/started from the assistant/)).toBeNull();
  });
});
