import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import PublishConfirmDialog from './PublishConfirmDialog';

afterEach(cleanup);

type Props = Parameters<typeof PublishConfirmDialog>[0];

function renderDialog(o: Partial<Props> = {}) {
  const props: Props = {
    title: 'M31 Project',
    count: 3,
    bytes: 142_000_000,
    needsApproval: false,
    coordinatorName: 'Ada',
    busy: false,
    error: null,
    onConfirm: vi.fn(),
    onCancel: vi.fn(),
    ...o,
  };
  render(<PublishConfirmDialog {...props} />);
  return props;
}

describe('PublishConfirmDialog', () => {
  it('renders through the shared dialog shell', () => {
    renderDialog();
    const d = screen.getByRole('dialog', { name: /Publish to M31 Project/ });
    expect(d.className).toContain('rounded-lg');
    expect(d.className).toMatch(/w-\[440px\]/);
  });

  it('states the count, the exact size and no approval line by default', () => {
    renderDialog();
    expect(screen.getByText('3 passing frames will be calibrated and announced to the project.')).toBeInTheDocument();
    expect(screen.getByText('Size 142 MB')).toBeInTheDocument();
    expect(screen.queryByText(/Estimated|≈/)).toBeNull();
    expect(screen.queryByText(/requires approval/)).toBeNull();
  });

  it('singular frame wording', () => {
    renderDialog({ count: 1 });
    expect(screen.getByText(/^1 passing frame will be/)).toBeInTheDocument();
  });

  it('shows the approval line only when needsApproval', () => {
    renderDialog({ needsApproval: true });
    expect(screen.getByText('This project requires approval — your contribution goes to Ada for review.')).toBeInTheDocument();
  });

  it('shows the error', () => {
    renderDialog({ error: 'hub said no' });
    expect(screen.getByText('hub said no')).toBeInTheDocument();
  });

  it('Publish calls onConfirm, Cancel calls onCancel; Publish takes initial focus', () => {
    const props = renderDialog();
    const publish = screen.getByRole('button', { name: 'Publish' });
    expect(publish).toHaveFocus();
    fireEvent.click(publish);
    expect(props.onConfirm).toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onCancel).toHaveBeenCalled();
  });

  it('busy locks Cancel, Publish and Escape', () => {
    const props = renderDialog({ busy: true });
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Publish' })).toBeDisabled();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(props.onCancel).not.toHaveBeenCalled();
  });
});
