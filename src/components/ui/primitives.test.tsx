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
  it('Card renders title, subtitle and action in one header row; the heading is named by the title alone', () => {
    render(<Card title="Exchange now" subtitle="43.4 MB/s" action={<button>Open Exchange →</button>}>body</Card>);
    const h = screen.getByRole('heading', { name: 'Exchange now' });
    expect(h.className).toContain('text-[13px]');
    expect(h).not.toHaveTextContent('43.4 MB/s');
    const row = h.parentElement!;
    expect(row.className).toContain('flex');
    expect(row).toHaveTextContent('43.4 MB/s');
    expect(row).toContainElement(screen.getByRole('button', { name: 'Open Exchange →' }));
  });
  it('Card with a null title renders no empty heading but keeps its action', () => {
    render(<Card title={null} action={<button>Open</button>}>body</Card>);
    expect(screen.queryByRole('heading')).toBeNull();
    expect(screen.getByRole('button', { name: 'Open' })).toBeInTheDocument();
  });
  it('Card with no title, subtitle or action renders no header row', () => {
    const { container } = render(<Card>body</Card>);
    expect((container.firstChild as HTMLElement).children).toHaveLength(0);
    expect(container.firstChild).toHaveTextContent('body');
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
  it('SegmentTiles renders a tile sub-label under its label', () => {
    render(<SegmentTiles tiles={[{ value: 'review', n: 2, label: 'To review', tone: 'purple', sub: '1h 00m · 1 night' }]} value="review" onChange={vi.fn()} />);
    expect(screen.getByText('1h 00m · 1 night')).toBeInTheDocument();
    expect(screen.getByText('2').className).toContain('text-purple');
  });
  it('Chip pur tone uses the purple token', () => {
    render(<Chip tone="pur">auto</Chip>);
    expect(screen.getByText('auto').className).toContain('text-purple');
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
  it('Sparkline drops non-finite samples instead of drawing NaN points', () => {
    const { container, rerender } = render(<Sparkline values={[1, NaN, 3, Infinity, 2]} color="#88c0d0" />);
    const pts = [...container.querySelectorAll('polyline')].map((p) => p.getAttribute('points') ?? '');
    expect(pts).toHaveLength(2);
    for (const p of pts) expect(p).not.toMatch(/NaN|Infinity/);
    // Three finite samples → three points on the line, spread over the full width.
    expect(pts[1].split(' ')).toHaveLength(3);
    expect(pts[1].split(' ')[2].startsWith('120.0,')).toBe(true);
    // Fewer than two finite samples draws nothing.
    rerender(<Sparkline values={[NaN, 4, -Infinity]} color="#88c0d0" />);
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

describe('Button tile size', () => {
  it("is the mockup's segment-tile row box: 33 px tall, min 150, 7x14 padding, radius 6, left-aligned (R34)", () => {
    render(<Button size="tile">+ Link an object</Button>);
    const cls = screen.getByRole('button', { name: '+ Link an object' }).className.split(/\s+/);
    expect(cls).toEqual(expect.arrayContaining(['h-[33px]', 'min-w-[150px]', 'rounded-md', 'px-3.5', 'py-[7px]', 'text-[12px]', 'border']));
    // One radius only (no base `rounded` to fight), and no centring.
    expect(cls).not.toContain('rounded');
    expect(cls).not.toContain('justify-center');
  });
  it('md and sm keep their 4 px radius', () => {
    render(<><Button>Plain</Button><Button size="sm">Small</Button></>);
    expect(screen.getByText('Plain').className.split(/\s+/)).toContain('rounded');
    expect(screen.getByText('Small').className.split(/\s+/)).toContain('rounded');
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

describe('Card flush', () => {
  it('runs content edge to edge and pads only the header (mockup table card)', () => {
    render(<Card flush title="Received">body</Card>);
    const section = screen.getByRole('heading', { name: 'Received' }).closest('section')!;
    expect(section.className).toContain('pt-3');
    expect(section.className).not.toContain('px-4');
    expect(screen.getByRole('heading', { name: 'Received' }).parentElement!.className).toContain('px-4');
  });
});
