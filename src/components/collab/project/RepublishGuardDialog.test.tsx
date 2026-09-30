import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import RepublishGuardDialog, { REPUBLISH_TYPED_CONFIRM_ABOVE } from './RepublishGuardDialog';

afterEach(cleanup);

type Props = Parameters<typeof RepublishGuardDialog>[0];

function renderGuard(o: Partial<Props> = {}) {
  const props: Props = {
    count: 2,
    sourceBytes: 2 * 1024 * 1024,
    all: false,
    busy: false,
    error: null,
    onConfirm: vi.fn(),
    onCancel: vi.fn(),
    ...o,
  };
  render(<RepublishGuardDialog {...props} />);
  return props;
}

const confirmButton = () => screen.getByRole('button', { name: 'Republish' });

describe('RepublishGuardDialog', () => {
  it('the typed-confirm threshold is 100 frames', () => {
    expect(REPUBLISH_TYPED_CONFIRM_ABOVE).toBe(100);
  });

  it('a selection states the count and source size, and confirms without typing', () => {
    const props = renderGuard({ count: 2, sourceBytes: 2 * 1024 * 1024 });
    expect(screen.getByRole('heading', { name: 'Republish 2 frames' })).toBeInTheDocument();
    expect(
      screen.getByText(
        '2 frames · 2.0 MB of source frames will be recalibrated. Every frame whose bytes change is posted as a new version, and every processor holding it downloads it again.',
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText('Every processor holding one of your frames re-downloads it once this finishes.'),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText(/to confirm/)).not.toBeInTheDocument();
    fireEvent.click(confirmButton());
    expect(props.onConfirm).toHaveBeenCalledTimes(1);
  });

  it('a 101-frame selection requires typing 101', () => {
    const props = renderGuard({ count: 101 });
    expect(confirmButton()).toBeDisabled();
    fireEvent.change(screen.getByLabelText('Type 101 to confirm'), { target: { value: '101' } });
    expect(confirmButton()).toBeEnabled();
    fireEvent.click(confirmButton());
    expect(props.onConfirm).toHaveBeenCalledTimes(1);
  });

  it('a 100-frame selection does not require typing', () => {
    renderGuard({ count: 100 });
    expect(screen.queryByLabelText(/to confirm/)).not.toBeInTheDocument();
    expect(confirmButton()).toBeEnabled();
  });

  it('a wrong number keeps Republish disabled', () => {
    const props = renderGuard({ count: 101 });
    const input = screen.getByLabelText('Type 101 to confirm');
    fireEvent.change(input, { target: { value: '100' } });
    expect(confirmButton()).toBeDisabled();
    fireEvent.change(input, { target: { value: '1010' } });
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(confirmButton());
    expect(props.onConfirm).not.toHaveBeenCalled();
  });

  it('"all" always needs the typed count, even for a few frames, under its own title', () => {
    renderGuard({ count: 3, all: true });
    expect(screen.getByRole('heading', { name: 'Recalibrate and republish all' })).toBeInTheDocument();
    expect(confirmButton()).toBeDisabled();
    fireEvent.change(screen.getByLabelText('Type 3 to confirm'), { target: { value: ' 3 ' } });
    expect(confirmButton()).toBeEnabled();
  });

  it('count 0 disables Republish with "Nothing to republish."', () => {
    renderGuard({ count: 0, sourceBytes: 0, all: true });
    expect(screen.getByText('Nothing to republish.')).toBeInTheDocument();
    expect(confirmButton()).toBeDisabled();
    expect(screen.queryByLabelText(/to confirm/)).not.toBeInTheDocument();
  });

  it('shows the error inline, and busy disables both buttons', () => {
    const props = renderGuard({ busy: true, error: 'hub unreachable' });
    expect(screen.getByText('hub unreachable')).toBeInTheDocument();
    expect(confirmButton()).toBeDisabled();
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(cancel).toBeDisabled();
    fireEvent.click(cancel);
    expect(props.onCancel).not.toHaveBeenCalled();
  });

  it('Cancel calls onCancel', () => {
    const props = renderGuard();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onCancel).toHaveBeenCalledTimes(1);
  });

  it('a single frame reads in the singular', () => {
    renderGuard({ count: 1, sourceBytes: 1024 });
    expect(screen.getByRole('heading', { name: 'Republish 1 frame' })).toBeInTheDocument();
    expect(screen.getByText(/^1 frame · 1\.0 KB of source frames/)).toBeInTheDocument();
  });

  it('renders through the shared dialog shell', () => {
    renderGuard();
    const d = screen.getByRole('dialog', { name: /Republish 2 frames/ });
    expect(d.className).toContain('rounded-lg');
    expect(d.className).toMatch(/w-\[440px\]/);
  });

  it('focuses the count field when typing is required, else Republish', () => {
    cleanup();
    renderGuard({ count: 2, all: true });
    expect(screen.getByLabelText('Type 2 to confirm')).toHaveFocus();
    cleanup();
    renderGuard({ count: 2 });
    expect(confirmButton()).toHaveFocus();
  });

  it('Escape cancels unless busy', () => {
    const props = renderGuard();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(props.onCancel).toHaveBeenCalledTimes(1);
    cleanup();
    const busy = renderGuard({ busy: true });
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(busy.onCancel).not.toHaveBeenCalled();
  });
});
