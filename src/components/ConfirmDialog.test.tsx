import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { ConfirmDialog } from './ConfirmDialog';

describe('ConfirmDialog on DialogShell', () => {
  it('renders as a labelled dialog with Cancel and the confirm action', () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(<ConfirmDialog isOpen title="Stop keeping 3 frames?" message={'line 1\nline 2'} confirmText="Stop keeping" confirmDanger onConfirm={onConfirm} onCancel={onCancel} />);
    expect(screen.getByRole('dialog', { name: 'Stop keeping 3 frames?' })).toBeInTheDocument();
    expect(screen.getByText(/line 1/).className).toContain('whitespace-pre-line');
    const confirm = screen.getByRole('button', { name: 'Stop keeping' });
    expect(confirm.className).toContain('bg-error');
    fireEvent.click(confirm);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
  it('renders nothing when closed', () => {
    render(<ConfirmDialog isOpen={false} title="t" message="m" onConfirm={() => {}} onCancel={() => {}} />);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('focuses Cancel for a danger confirm and the confirm button otherwise', () => {
    const { unmount } = render(<ConfirmDialog isOpen title="t" message="m" confirmText="Delete" confirmDanger onConfirm={() => {}} onCancel={() => {}} />);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Cancel' }));
    unmount();
    render(<ConfirmDialog isOpen title="t" message="m" confirmText="Go" onConfirm={() => {}} onCancel={() => {}} />);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Go' }));
  });
});
