import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { Checkbox } from './Checkbox';

describe('Checkbox', () => {
  it('renders the accent-tinted native input reflecting the checked prop', () => {
    render(<Checkbox checked={false} onChange={() => {}} label="Auto check" />);
    const input = screen.getByRole('checkbox', { name: 'Auto check' });
    expect(input).not.toBeChecked();
    expect(input.className).toContain('accent-accent');
  });

  it('calls onChange with the new value when clicked', () => {
    const onChange = vi.fn();
    render(<Checkbox checked={false} onChange={onChange} label="Auto check" />);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Auto check' }));
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith(true);
  });

  it('renders as a switch and at the sm size when asked', () => {
    render(<Checkbox checked role="switch" size="sm" onChange={() => {}} label="Watch for new files" />);
    const input = screen.getByRole('switch', { name: 'Watch for new files' });
    expect(input).toBeChecked();
    expect(input.className).toContain('w-3.5');
    expect(input.className).toContain('h-3.5');
  });

  it('defaults to the md size', () => {
    render(<Checkbox checked={false} onChange={() => {}} label="Auto check" />);
    const input = screen.getByRole('checkbox', { name: 'Auto check' });
    expect(input.className).toContain('w-4');
    expect(input.className).toContain('h-4');
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
});
