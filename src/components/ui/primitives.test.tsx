import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Bar, Button, Pill, StatusDot, Card, Chip, EmptyState, FilterChip, KV, ProgressBar, Seg, SegmentTiles, Sparkline, TextArea } from '.';
import { createRef } from 'react';

describe('ui primitives', () => {
  it('TextArea forwards its ref and renders a textarea with the field style', () => {
    const ref = createRef<HTMLTextAreaElement>();
    render(<TextArea ref={ref} rows={4} placeholder="why" />);
    expect(ref.current).toBeInstanceOf(HTMLTextAreaElement);
    expect(screen.getByPlaceholderText('why').className).toContain('bg-surface-elevated');
  });
  it('Button variants carry the mockup classes', () => {
    render(<><Button>Plain</Button><Button variant="primary">Go</Button><Button size="sm">Small</Button></>);
    expect(screen.getByText('Plain').className).toContain('text-[12px]');
    expect(screen.getByText('Go').className).toContain('bg-accent');
    expect(screen.getByText('Small').className).toContain('text-[11px]');
  });
  it('Chip tones map to the muted backgrounds', () => {
    render(<Chip tone="err">missing</Chip>);
    expect(screen.getByText('missing').className).toContain('bg-error-muted');
  });
  it('Card renders title, subtitle and action in one header', () => {
    render(<Card title="Receiving" subtitle="43.4 MB/s" action={<button>x</button>}>body</Card>);
    const h = screen.getByRole('heading', { name: /Receiving/ });
    expect(h).toHaveTextContent('43.4 MB/s');
    expect(h.className).toContain('text-[13px]');
  });
  it('KV renders a definition list with faint terms', () => {
    render(<KV items={[['Night', '2026-08-31 · Mon']]} />);
    expect(screen.getByText('Night').tagName).toBe('DT');
    expect(screen.getByText('2026-08-31 · Mon').tagName).toBe('DD');
  });
  it('Seg and SegmentTiles report the chosen value', () => {
    const onSeg = vi.fn();
    const onTile = vi.fn();
    render(<>
      <Seg options={[{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }]} value="a" onChange={onSeg} />
      <SegmentTiles tiles={[{ value: 'ready', n: 136, label: 'Ready to publish', tone: 'accent' }, { value: 'held', n: 390, label: 'Held back', tone: 'warning' }]} value="ready" onChange={onTile} />
    </>);
    fireEvent.click(screen.getByText('B'));
    fireEvent.click(screen.getByText('Held back'));
    expect(onSeg).toHaveBeenCalledWith('b');
    expect(onTile).toHaveBeenCalledWith('held');
    expect(screen.getByRole('button', { name: /136 Ready to publish/ })).toHaveAttribute('aria-pressed', 'true');
  });
  it('Bar sizes its segments as shares of the total', () => {
    const { container } = render(<Bar segments={[{ value: 3, color: '#a3be8c' }, { value: 1, color: '#bf616a' }]} />);
    const segs = container.querySelectorAll('i');
    expect((segs[0] as HTMLElement).style.width).toBe('75%');
    expect((segs[1] as HTMLElement).style.width).toBe('25%');
  });
  it('ProgressBar clamps to 0–100', () => {
    const { container } = render(<ProgressBar percent={140} />);
    expect((container.querySelector('i') as HTMLElement).style.width).toBe('100%');
  });
  it('Sparkline draws a line for 2+ samples and nothing for fewer', () => {
    const { container, rerender } = render(<Sparkline values={[1, 3, 2]} color="#88c0d0" />);
    expect(container.querySelectorAll('polyline')).toHaveLength(2);
    rerender(<Sparkline values={[1]} color="#88c0d0" />);
    expect(container.querySelector('polyline')).toBeNull();
  });
  it('FilterChip shows count and dims at zero', () => {
    render(<FilterChip filter="Ha" count={0} on={false} onClick={() => {}} />);
    expect(screen.getByRole('button', { name: /Ha 0/ }).className).toContain('opacity-40');
  });
  it('EmptyState uses the faint 12.5px line', () => {
    render(<EmptyState>Nothing is moving.</EmptyState>);
    expect(screen.getByText('Nothing is moving.').className).toContain('text-[12.5px]');
  });
  it('disabled Pill button dims and ignores clicks', () => {
    const onClick = vi.fn();
    render(<Pill as="button" disabled onClick={onClick}>Live</Pill>);
    const b = screen.getByRole('button', { name: 'Live' });
    expect(b).toBeDisabled();
    expect(b.className).toContain('opacity-45');
    fireEvent.click(b);
    expect(onClick).not.toHaveBeenCalled();
  });
  it('live StatusDot uses a token ring, no raw rgba', () => {
    const { container } = render(<StatusDot state="live" />);
    const cls = (container.firstChild as HTMLElement).className;
    expect(cls).toContain('ring-success/[0.18]');
    expect(cls).not.toContain('rgba(');
  });
});

describe('Button link variant', () => {
  it('keeps the mockup link geometry: UA button padding 1px 6px', () => {
    render(<Button variant="link">Expand all</Button>);
    const b = screen.getByRole('button', { name: 'Expand all' });
    expect(b.className).toContain('px-1.5');
    expect(b.className).toContain('py-px');
  });
});
