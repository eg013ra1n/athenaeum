import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { TransferRow } from './TransferRow';
import type { UnifiedRow } from './types';
import type { ReceiveSessionView } from '../../types/models';

function receiveSession(overrides: Partial<ReceiveSessionView> = {}): ReceiveSessionView {
  return {
    id: 1,
    projectId: 'proj-1',
    projectTitle: 'M31 Deep Field',
    startedAt: '2026-09-30T10:00:00Z',
    finishedAt: '2026-09-30T10:05:00Z',
    frames: 48,
    bytes: 1_000_000,
    failed: 0,
    sources: [
      { device: 'd1', memberName: 'Kostya', deviceName: null, bytes: 500_000 },
      { device: 'd2', memberName: 'Olga', deviceName: null, bytes: 500_000 },
    ],
    ...overrides,
  };
}

const noop = () => {};

function renderRow(session: ReceiveSessionView) {
  const item: UnifiedRow = { kind: 'session', selKey: `session:${session.id}`, session };
  return render(
    <TransferRow
      item={item}
      selected={false}
      onSelect={noop}
      now={Date.now()}
      busy={new Set()}
      onSendNow={noop}
      onCancelOutbound={noop}
      onCancelInbound={noop}
      onResend={noop}
      onDelete={noop}
    />,
  );
}

describe('TransferRow — session', () => {
  it('renders "from Kostya, Olga" and "48 frames"', () => {
    renderRow(receiveSession());

    expect(screen.getByText(/from Kostya, Olga/)).toBeInTheDocument();
    expect(screen.getByText(/48 frames/)).toBeInTheDocument();
  });

  it('renders the project title and no action buttons', () => {
    renderRow(receiveSession());

    expect(screen.getByText('M31 Deep Field')).toBeInTheDocument();
    // Only the row's own selection wrapper carries a button role — no
    // Send now / Cancel / Resend / Delete affordance for a session row.
    expect(screen.getAllByRole('button')).toHaveLength(1);
  });

  it('shows a failed count when the session had failures', () => {
    renderRow(receiveSession({ failed: 2 }));

    expect(screen.getByText(/2 failed/)).toBeInTheDocument();
  });
});
