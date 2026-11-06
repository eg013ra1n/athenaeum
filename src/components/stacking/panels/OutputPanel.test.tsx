// Wave 6c Task 8: the Output panel's two coverage fields — the coverage map beside each master and the crop to the
// rectangle a minimum number of frames cover. Both are config fields; what they start at (a project on / 0, a frame
// set off / 0) comes from the stored config, not from this panel.
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { OutputPanel } from './OutputPanel';
import type { StackingConfig } from '../../../types/stacking';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn().mockResolvedValue({ working: { effective: '/w' }, output: { effective: '/o' } }) } }));
vi.mock('../../../api/desktop', () => ({ pickDirectory: vi.fn() }));
vi.mock('../../FolderBrowserModal', () => ({ FolderBrowserModal: () => null }));

const MAP_LABEL = 'Write a coverage map (frames per pixel) beside each master';
const CROP_LABEL = 'Crop to coverage of at least';

const config = (output: Partial<StackingConfig['output']> = {}): StackingConfig =>
  ({
    paths: { workingDir: null, outputDir: null },
    output: { format: 'fits', cleanup: 'keepAll', coverageMap: false, cropToCoverage: 0, ...output },
  }) as unknown as StackingConfig;

const show = (
  c: StackingConfig = config(),
  over: { disabled?: boolean; mode?: 'perSet' | 'global' } = {},
) => {
  const onChange = vi.fn();
  render(<OutputPanel config={c} onChange={onChange} plan={null} disabled={over.disabled} mode={over.mode ?? 'perSet'} />);
  return onChange;
};

/** The one config the panel handed to `onChange`. */
const sent = (onChange: ReturnType<typeof vi.fn>) => {
  expect(onChange).toHaveBeenCalledTimes(1);
  return (onChange.mock.calls[0]?.[0] as StackingConfig).output;
};

describe('OutputPanel coverage fields', () => {
  it('starts from the stored config: a project’s map on and crop 0, a frame set’s map off and crop 0', () => {
    const { unmount } = render(
      <OutputPanel config={config({ coverageMap: true })} onChange={vi.fn()} plan={null} mode="global" />,
    );
    expect(screen.getByRole('checkbox', { name: MAP_LABEL })).toBeChecked();
    expect((screen.getByLabelText(CROP_LABEL) as HTMLInputElement).value).toBe('0');
    unmount();
    show(config({ coverageMap: false }), { mode: 'global' });
    expect(screen.getByRole('checkbox', { name: MAP_LABEL })).not.toBeChecked();
    expect((screen.getByLabelText(CROP_LABEL) as HTMLInputElement).value).toBe('0');
  });

  it('shows the crop field as a whole number of frames with the 0 = no crop hint', () => {
    show();
    const field = screen.getByLabelText(CROP_LABEL);
    expect(field).toHaveAttribute('type', 'number');
    expect(field).toHaveAttribute('min', '0');
    expect(field).toHaveAttribute('step', '1');
    expect(screen.getByText('frames (0 = no crop)')).toBeInTheDocument();
  });

  it('shows both fields in the per-set panel and the global one', () => {
    for (const mode of ['perSet', 'global'] as const) {
      const { unmount } = render(<OutputPanel config={config()} onChange={vi.fn()} plan={null} mode={mode} />);
      expect(screen.getByRole('checkbox', { name: MAP_LABEL }), mode).toBeInTheDocument();
      expect(screen.getByLabelText(CROP_LABEL), mode).toBeInTheDocument();
      unmount();
    }
  });

  it('patches coverageMap when the box is ticked, and leaves the rest of the output alone', () => {
    const onChange = show(config({ cropToCoverage: 2 }));
    fireEvent.click(screen.getByRole('checkbox', { name: MAP_LABEL }));
    expect(sent(onChange)).toEqual({ format: 'fits', cleanup: 'keepAll', coverageMap: true, cropToCoverage: 2 });
  });

  it('patches coverageMap off when a project’s default is unticked', () => {
    const onChange = show(config({ coverageMap: true }));
    fireEvent.click(screen.getByRole('checkbox', { name: MAP_LABEL }));
    expect(sent(onChange).coverageMap).toBe(false);
  });

  it('patches cropToCoverage with the typed number of frames', () => {
    const onChange = show();
    fireEvent.change(screen.getByLabelText(CROP_LABEL), { target: { value: '3' } });
    expect(sent(onChange)).toEqual({ format: 'fits', cleanup: 'keepAll', coverageMap: false, cropToCoverage: 3 });
  });

  // `NumericField` holds the typed value to `min`, so a -1 reaches the config as 0 (no crop), never as a negative.
  it('refuses -1: the crop never goes below 0', () => {
    const onChange = show(config({ cropToCoverage: 4 }));
    fireEvent.change(screen.getByLabelText(CROP_LABEL), { target: { value: '-1' } });
    expect(sent(onChange).cropToCoverage).toBe(0);
  });

  // `crop_to_coverage` is a `u32` on the wire: a fraction would fail to deserialize.
  it('rounds a fractional crop to a whole number of frames', () => {
    const onChange = show();
    fireEvent.change(screen.getByLabelText(CROP_LABEL), { target: { value: '2.6' } });
    expect(sent(onChange).cropToCoverage).toBe(3);
  });

  it('disables both while a run is active', () => {
    show(config(), { disabled: true });
    expect(screen.getByRole('checkbox', { name: MAP_LABEL })).toBeDisabled();
    expect(screen.getByLabelText(CROP_LABEL)).toBeDisabled();
  });
});
