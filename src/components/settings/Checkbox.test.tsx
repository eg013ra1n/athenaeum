import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { Checkbox } from './Checkbox';

describe('Checkbox', () => {
  it('renders a real input reflecting the checked prop', () => {
    render(<Checkbox checked={false} onChange={() => {}} label="Auto check" />);
    expect(screen.getByRole('checkbox', { name: 'Auto check' })).not.toBeChecked();
  });

  it('calls onChange with the new value when clicked', () => {
    const onChange = vi.fn();
    render(<Checkbox checked={false} onChange={onChange} label="Auto check" />);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Auto check' }));
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith(true);
  });

  it('renders as a switch at the sm size without changing the box', () => {
    render(<Checkbox checked role="switch" size="sm" onChange={() => {}} label="Watch for new files" />);
    expect(screen.getByRole('switch', { name: 'Watch for new files' })).toBeChecked();
    expect(screen.getByTestId('cb-box').className).toContain('h-[13px]');
  });

  it('ariaLabel names the control over its visible label', () => {
    render(<Checkbox checked role="switch" onChange={() => {}} label="On" ariaLabel="Auto-replicate" />);
    expect(screen.getByRole('switch', { name: 'Auto-replicate' })).toBeChecked();
    expect(screen.getByText('On')).toBeInTheDocument();
  });

  it('draws the 13px box and a tick only when checked', () => {
    const { rerender } = render(<Checkbox checked={false} onChange={() => {}} label="Auto check" />);
    const box = screen.getByTestId('cb-box');
    expect(box.className).toContain('h-[13px]');
    expect(box.className).toContain('w-[13px]');
    expect(box.querySelector('svg')).toBeNull();
    rerender(<Checkbox checked onChange={() => {}} label="Auto check" />);
    expect(screen.getByTestId('cb-box').querySelector('svg')).not.toBeNull();
  });

  it('renders the description under the label', () => {
    render(<Checkbox checked={false} onChange={() => {}} label="Auto check" description="Checks daily in the background." />);
    expect(screen.getByText('Checks daily in the background.')).toBeInTheDocument();
  });

  it('disables the input and dims the label when disabled', () => {
    render(<Checkbox checked={false} onChange={() => {}} label="Auto check" disabled />);
    const input = screen.getByRole('checkbox', { name: 'Auto check' });
    expect(input).toBeDisabled();
    expect(input.closest('label')?.className).toContain('opacity-50');
  });

  it('positions the label, toggles from the box, and does not double-dim when disabled', () => {
    const onChange = vi.fn();
    const { rerender } = render(<Checkbox checked={false} onChange={onChange} label="Auto check" />);
    expect(screen.getByRole('checkbox').closest('label')?.className.split(' ')).toContain('relative');
    fireEvent.click(screen.getByTestId('cb-box'));
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith(true);
    rerender(<Checkbox checked={false} onChange={onChange} label="Auto check" disabled />);
    expect(screen.getByTestId('cb-box').className).not.toContain('opacity-45');
  });
});
