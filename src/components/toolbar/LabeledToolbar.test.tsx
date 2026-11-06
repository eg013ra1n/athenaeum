import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { BlinkToolButton, ToolButton, ToolDivider, ToolSelection, ToolSplit, ToolToggle } from './LabeledToolbar';

describe('LabeledToolbar', () => {
  it('blink_label_follows_selection', () => {
    const { rerender } = render(<BlinkToolButton selectedCount={0} visibleCount={4} onClick={() => {}} />);
    expect(screen.getByText('Blink all')).toBeInTheDocument();
    expect(screen.getByTitle('Blink all 4 frames')).toBeInTheDocument();

    rerender(<BlinkToolButton selectedCount={2} visibleCount={4} onClick={() => {}} />);
    expect(screen.getByText('Blink')).toBeInTheDocument();
    expect(screen.queryByText('Blink all')).toBeNull();
    expect(screen.getByTitle('Blink 2 selected frames')).toBeInTheDocument();

    rerender(<BlinkToolButton selectedCount={1} visibleCount={1} onClick={() => {}} />);
    expect(screen.getByTitle('Blink 1 selected frame')).toBeInTheDocument();

    rerender(<BlinkToolButton selectedCount={0} visibleCount={1} onClick={() => {}} />);
    expect(screen.getByTitle('Blink all 1 frame')).toBeInTheDocument();

    rerender(<BlinkToolButton selectedCount={0} visibleCount={0} onClick={() => {}} />);
    expect(screen.getByTitle('Blink all 0 frames')).toBeDisabled();
  });

  it('blink_click_calls_back', () => {
    const onClick = vi.fn();
    render(<BlinkToolButton selectedCount={0} visibleCount={4} onClick={onClick} />);
    fireEvent.click(screen.getByTitle('Blink all 4 frames'));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it('blink_uses_token_classes', () => {
    render(<BlinkToolButton selectedCount={0} visibleCount={4} onClick={() => {}} />);
    const cls = screen.getByTitle('Blink all 4 frames').className;
    expect(cls).toContain('bg-blink');
    expect(cls).toContain('hover:bg-blink-hover');
    expect(cls).not.toContain('cyan');
  });

  it('split_marks_active_segment', () => {
    const onChange = vi.fn();
    render(
      <ToolSplit
        label="Group by"
        value="night"
        onChange={onChange}
        options={[
          { value: 'night', title: 'Group by night', content: 'Night' },
          { value: 'camera', title: 'Group by camera', content: 'Camera' },
        ]}
      />,
    );
    const night = screen.getByTitle('Group by night');
    const camera = screen.getByTitle('Group by camera');
    expect(night).toHaveAttribute('aria-pressed', 'true');
    expect(night.className).toContain('bg-accent/20');
    expect(night.className).toContain('rounded-l-lg');
    expect(night.className).toContain('border-r-0');
    expect(camera).toHaveAttribute('aria-pressed', 'false');
    expect(camera.className).not.toContain('bg-accent/20');
    expect(camera.className).toContain('bg-surface-hover');
    expect(camera.className).toContain('rounded-r-lg');
    expect(screen.getByText('Group by')).toBeInTheDocument();

    fireEvent.click(camera);
    expect(onChange).toHaveBeenCalledWith('camera');
  });

  it('split_without_a_value_marks_no_segment', () => {
    render(
      <ToolSplit
        label="Group by"
        value={null}
        onChange={() => {}}
        options={[
          { value: 'a', title: 'A', content: 'A' },
          { value: 'b', title: 'B', content: 'B' },
        ]}
      />,
    );
    expect(screen.getByTitle('A')).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByTitle('B')).toHaveAttribute('aria-pressed', 'false');
  });

  it('split_disabled_disables_every_segment', () => {
    const onChange = vi.fn();
    render(
      <ToolSplit
        label="Group by"
        value="a"
        disabled
        onChange={onChange}
        options={[
          { value: 'a', title: 'A', content: 'A' },
          { value: 'b', title: 'B', content: 'B' },
        ]}
      />,
    );
    expect(screen.getByTitle('A')).toBeDisabled();
    expect(screen.getByTitle('B')).toBeDisabled();
    fireEvent.click(screen.getByTitle('B'));
    expect(onChange).not.toHaveBeenCalled();
  });

  it('selection_count_clears', () => {
    const onClear = vi.fn();
    const onInvert = vi.fn();
    render(<ToolSelection count={3} onClear={onClear} onInvert={onInvert} />);
    expect(screen.getByText('Selection')).toBeInTheDocument();
    const clear = screen.getByTitle('Clear selection');
    expect(clear).toHaveTextContent('3');
    fireEvent.click(clear);
    expect(onClear).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTitle('Invert selection'));
    expect(onInvert).toHaveBeenCalledTimes(1);
  });

  it('selection_with_nothing_selected_cannot_clear_but_can_invert', () => {
    const onClear = vi.fn();
    const onInvert = vi.fn();
    render(<ToolSelection count={0} onClear={onClear} onInvert={onInvert} />);
    const clear = screen.getByTitle('Clear selection');
    expect(clear).toHaveTextContent('0');
    expect(clear).toBeDisabled();
    expect(screen.getByTitle('Invert selection')).toBeEnabled();
  });

  it('toggle_reflects_on_state', () => {
    const onClick = vi.fn();
    const { rerender } = render(<ToolToggle label="Units" title="Toggle units" on={false} onClick={onClick}>px</ToolToggle>);
    const off = screen.getByTitle('Toggle units');
    expect(off).toHaveAttribute('aria-pressed', 'false');
    expect(off.className).toContain('bg-surface-hover');
    expect(off.className).not.toContain('bg-accent/20');
    expect(screen.getByText('Units')).toBeInTheDocument();
    fireEvent.click(off);
    expect(onClick).toHaveBeenCalledTimes(1);

    rerender(<ToolToggle label="Units" title="Toggle units" on onClick={onClick}>px</ToolToggle>);
    const on = screen.getByTitle('Toggle units');
    expect(on).toHaveAttribute('aria-pressed', 'true');
    expect(on.className).toContain('bg-accent/20');
    expect(on.className).toContain('border-accent');
  });

  it('toggle_disabled_does_not_fire', () => {
    const onClick = vi.fn();
    render(<ToolToggle label="Units" title="Toggle units" on={false} disabled onClick={onClick}>px</ToolToggle>);
    fireEvent.click(screen.getByTitle('Toggle units'));
    expect(onClick).not.toHaveBeenCalled();
  });

  it('button_keeps_its_label_and_default_class', () => {
    const onClick = vi.fn();
    render(<ToolButton label="CSV" title="Export CSV" onClick={onClick}>x</ToolButton>);
    const btn = screen.getByRole('button', { name: 'CSV' });
    expect(btn).toHaveAttribute('title', 'Export CSV');
    expect(btn.className).toContain('text-content-secondary');
    fireEvent.click(btn);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it('divider_is_a_bar', () => {
    const { container } = render(<ToolDivider />);
    expect(container.firstChild).toHaveTextContent('|');
    expect((container.firstChild as HTMLElement).className).toBe('text-border h-7 flex items-center');
  });
});
