// Wave 6c Task 7: the canonical grid card, its dialog and the schematic. The grid is the project's one registration
// target (spec §6): an editor proposes, confirms and changes it here; everyone else reads it.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactElement } from 'react';
import { api } from '../../api';
import { GridCard } from './GridCard';
import { GridDialog, validateGrid } from './GridDialog';
import { GridSchematic } from './GridSchematic';
import { MemberColorsProvider } from '../collab/project/MemberColorsContext';
import { memberColor } from '../collab/project/memberColors';
import type { CanonicalGrid } from '../../types/models';
import type { GridState, PlanGrid, PlanGridFootprint, PlanGridTarget } from '../../types/stacking';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const GRID: CanonicalGrid = { scaleArcsec: 2, widthPx: 4096, heightPx: 3072, crval1: 83.8, crval2: -5.4, rotationDeg: 0 };
const PROPOSAL: CanonicalGrid = { scaleArcsec: 1.5, widthPx: 3200, heightPx: 2560, crval1: 83.82, crval2: -5.39, rotationDeg: 0 };
const TARGET: PlanGridTarget = { raDeg: 83.82, decDeg: -5.39, radiusDeg: 1 };

const MEMBERS = [
  { accountId: 'acc-a', displayName: 'Alice' },
  { accountId: 'acc-b', displayName: 'Bob' },
  { accountId: 'acc-c', displayName: 'Cleo' },
];

/** A small square around the target, RA/Dec degrees in perimeter order. */
function squareAround(ra: number, dec: number, half = 0.2): [number, number][] {
  const dRa = half / Math.cos((dec * Math.PI) / 180);
  return [
    [ra - dRa, dec + half], [ra + dRa, dec + half], [ra + dRa, dec - half], [ra - dRa, dec - half],
  ];
}

const footprint = (uuid: string, accountId: string, outline: [number, number][]): PlanGridFootprint => ({
  frameUuid: uuid, publisherAccountId: accountId, publisher: accountId, filterCanonical: 'L', outline,
});

const FOOTPRINTS: PlanGridFootprint[] = [
  footprint('f1', 'acc-a', squareAround(83.8, -5.4)),
  footprint('f2', 'acc-b', squareAround(84.0, -5.2)),
  footprint('f3', 'acc-c', squareAround(83.6, -5.6)),
];

const planGrid = (state: GridState, over: Partial<PlanGrid> = {}): PlanGrid => ({
  state, current: state === 'confirmed' ? GRID : null, proposal: PROPOSAL, framesInside: 12, canEdit: true,
  target: TARGET, footprints: FOOTPRINTS, framesNotDrawn: 0, ...over,
});

const withMembers = (el: ReactElement) => (
  <MemberColorsProvider members={MEMBERS} selfAccountId="acc-a">{el}</MemberColorsProvider>
);

const outlines = () => screen.getAllByTestId('grid-outline');

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockResolvedValue(undefined as never);
  notifyMock.mockClear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('GridSchematic', () => {
  const draw = (over: Partial<Parameters<typeof GridSchematic>[0]> = {}) =>
    render(
      withMembers(<GridSchematic grid={GRID} target={TARGET} footprints={FOOTPRINTS} framesNotDrawn={0} {...over} />),
    );

  it('draws one outline per footprint, each in its publisher colour', () => {
    draw();
    expect(outlines()).toHaveLength(3);
    const colours = outlines().map((o) => o.getAttribute('stroke'));
    expect(colours).toEqual([
      memberColor('acc-a', MEMBERS, 'acc-a'),
      memberColor('acc-b', MEMBERS, 'acc-a'),
      memberColor('acc-c', MEMBERS, 'acc-a'),
    ]);
    for (const o of outlines()) expect(o.getAttribute('fill')).toBe('none');
  });

  it('gives an outline with no known publisher the muted fallback class and the text colour as stroke', () => {
    render(<GridSchematic grid={GRID} target={TARGET} footprints={FOOTPRINTS} framesNotDrawn={0} />);
    expect(outlines()).toHaveLength(3);
    for (const o of outlines()) {
      expect(o.getAttribute('class')).toContain('text-content-muted');
      expect(o.getAttribute('stroke')).toBe('currentColor');
    }
  });

  it('draws the grid rectangle in the accent stroke and the target circle dashed', () => {
    draw();
    expect(screen.getByTestId('grid-rect').getAttribute('class')).toContain('stroke-accent');
    const target = screen.getByTestId('grid-target');
    expect(target.tagName.toLowerCase()).toBe('circle');
    expect(target.getAttribute('stroke-dasharray')).toBeTruthy();
  });

  it('spans the grid rectangle over the pixel edges, grid-up', () => {
    draw();
    const rect = screen.getByTestId('grid-rect');
    expect(Number(rect.getAttribute('x'))).toBeCloseTo(-0.5, 9);
    expect(Number(rect.getAttribute('width'))).toBe(4096);
    expect(Number(rect.getAttribute('height'))).toBe(3072);
  });

  it('does not draw an outline with a point behind the tangent plane', () => {
    const behind = footprint('behind', 'acc-b', [...squareAround(83.8, -5.4), [GRID.crval1 + 100, GRID.crval2]]);
    draw({ footprints: [FOOTPRINTS[0]!, behind, FOOTPRINTS[2]!] });
    expect(outlines().map((o) => o.getAttribute('data-frame-uuid'))).toEqual(['f1', 'f3']);
  });

  it('draws an outline that straddles RA 0 on the right side of the grid, never mirrored', () => {
    const wrapGrid: CanonicalGrid = { ...GRID, crval1: 0, crval2: 10 };
    const dRa = 0.2 / Math.cos((10 * Math.PI) / 180);
    const across = footprint('wrap', 'acc-b', [
      [360 - dRa, 10.2], [dRa, 10.2], [dRa, 9.8], [360 - dRa, 9.8],
    ]);
    draw({ grid: wrapGrid, target: { raDeg: 0, decDeg: 10, radiusDeg: 1 }, footprints: [across] });
    const xs = (outlines()[0]!.getAttribute('points') ?? '')
      .trim()
      .split(/\s+/)
      .map((p) => Number(p.split(',')[0]));
    expect(xs).toHaveLength(4);
    // Centre pixel column is (4096 - 1) / 2; the frame is 0.4 degrees wide, i.e. 720 px at 2"/px, centred on it.
    for (const x of xs) expect(Math.abs(x - 2047.5)).toBeLessThan(400);
    expect(Math.max(...xs) - Math.min(...xs)).toBeGreaterThan(700);
  });

  it('extends the view box to a frame that reaches past the grid', () => {
    const far = footprint('far', 'acc-b', squareAround(GRID.crval1 + 2, GRID.crval2, 0.2));
    const small = render(withMembers(<GridSchematic grid={GRID} target={TARGET} footprints={[]} framesNotDrawn={0} />));
    const smallBox = small.getByTestId('grid-schematic').getAttribute('viewBox')!.split(' ').map(Number);
    small.unmount();
    draw({ footprints: [far] });
    const box = screen.getByTestId('grid-schematic').getAttribute('viewBox')!.split(' ').map(Number);
    expect(box[2]!).toBeGreaterThan(smallBox[2]!);
  });

  it('keeps the view box at the grid rectangle plus 5 percent when every outline is inside it', () => {
    draw({ footprints: [] });
    const [x, , w, h] = screen.getByTestId('grid-schematic').getAttribute('viewBox')!.split(' ').map(Number);
    expect(w).toBeCloseTo(4096 * 1.1, 6);
    expect(h).toBeCloseTo(3072 * 1.1, 6);
    expect(x).toBeCloseTo(-0.5 - 4096 * 0.05, 6);
  });

  it('is at most 220 px high', () => {
    draw();
    expect(screen.getByTestId('grid-schematic').style.maxHeight).toBe('220px');
  });

  it('says how many frames are not drawn, and nothing when all are', () => {
    const { unmount } = draw({ framesNotDrawn: 4 });
    expect(screen.getByText('4 frames are not drawn: no WCS in the manifest')).toBeInTheDocument();
    unmount();
    draw({ framesNotDrawn: 0 });
    expect(screen.queryByText(/not drawn/)).toBeNull();
  });

  it('says it in the singular for one frame', () => {
    draw({ framesNotDrawn: 1 });
    expect(screen.getByText('1 frame is not drawn: no WCS in the manifest')).toBeInTheDocument();
  });

  it('tilts the N arrow by the rotation of the grid', () => {
    const { unmount } = draw();
    expect(screen.getByTestId('grid-north').getAttribute('data-angle')).toBe('0.0');
    unmount();
    draw({ grid: { ...GRID, rotationDeg: 30 } });
    expect(screen.getByTestId('grid-north').getAttribute('data-angle')).toBe('-30.0');
    expect(screen.getByText('N')).toBeInTheDocument();
  });

  it('draws the target circle and the outlines on a tangent plane at the target when there is no grid', () => {
    draw({ grid: null });
    expect(screen.queryByTestId('grid-rect')).toBeNull();
    expect(screen.getByTestId('grid-target')).toBeInTheDocument();
    expect(outlines()).toHaveLength(3);
  });
});

describe('validateGrid', () => {
  const bad = (over: Partial<CanonicalGrid>) => validateGrid({ ...GRID, ...over });

  it('accepts a grid inside the bounds', () => expect(validateGrid(GRID)).toBeNull());

  it('refuses a scale of 0 and allows 100', () => {
    expect(bad({ scaleArcsec: 0 })).not.toBeNull();
    expect(bad({ scaleArcsec: 100 })).toBeNull();
    expect(bad({ scaleArcsec: 100.01 })).not.toBeNull();
    expect(bad({ scaleArcsec: -1 })).not.toBeNull();
  });

  it('refuses a width or height of 63 and allows 64 and 65536', () => {
    expect(bad({ widthPx: 63 })).not.toBeNull();
    expect(bad({ widthPx: 64 })).toBeNull();
    expect(bad({ widthPx: 65536 })).toBeNull();
    expect(bad({ widthPx: 65537 })).not.toBeNull();
    expect(bad({ heightPx: 63 })).not.toBeNull();
    expect(bad({ heightPx: 65536 })).toBeNull();
    expect(bad({ heightPx: 65537 })).not.toBeNull();
  });

  it('refuses a width or height that is not a whole number of pixels', () => {
    expect(bad({ widthPx: 100.5 })).not.toBeNull();
    expect(bad({ heightPx: 99.5 })).not.toBeNull();
  });

  it('refuses a centre RA of 360 and allows 0 and just under 360', () => {
    expect(bad({ crval1: 360 })).not.toBeNull();
    expect(bad({ crval1: 0 })).toBeNull();
    expect(bad({ crval1: 359.9999 })).toBeNull();
    expect(bad({ crval1: -0.1 })).not.toBeNull();
  });

  it('allows a centre Dec of -90 and 90 and refuses beyond', () => {
    expect(bad({ crval2: -90 })).toBeNull();
    expect(bad({ crval2: 90 })).toBeNull();
    expect(bad({ crval2: -90.1 })).not.toBeNull();
    expect(bad({ crval2: 90.1 })).not.toBeNull();
  });

  it('refuses a rotation that is not finite and allows any finite one', () => {
    expect(bad({ rotationDeg: NaN })).not.toBeNull();
    expect(bad({ rotationDeg: Infinity })).not.toBeNull();
    expect(bad({ rotationDeg: 540 })).toBeNull();
  });

  it('refuses a NaN in any field', () => {
    expect(bad({ scaleArcsec: NaN })).not.toBeNull();
    expect(bad({ widthPx: NaN })).not.toBeNull();
    expect(bad({ heightPx: NaN })).not.toBeNull();
    expect(bad({ crval1: NaN })).not.toBeNull();
    expect(bad({ crval2: NaN })).not.toBeNull();
  });
});

describe('GridDialog', () => {
  const SCHEMATIC = { target: TARGET, footprints: FOOTPRINTS, framesNotDrawn: 0 };

  const open = (over: Partial<Parameters<typeof GridDialog>[0]> = {}) => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    const view = render(
      withMembers(
        <GridDialog initial={GRID} warnRewrite={false} schematic={SCHEMATIC} onSave={onSave} onClose={onClose} {...over} />,
      ),
    );
    return { onSave, onClose, ...view };
  };

  const field = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
  const set = (label: string, value: string) => fireEvent.change(field(label), { target: { value } });
  const save = () => screen.getByRole('button', { name: 'Save' });

  it('prefills every field from the grid', () => {
    open();
    expect(field('Scale (″/px)').value).toBe('2');
    expect(field('Width (px)').value).toBe('4096');
    expect(field('Height (px)').value).toBe('3072');
    expect(field('Centre RA (°)').value).toBe('83.8');
    expect(field('Centre Dec (°)').value).toBe('-5.4');
    expect(field('Rotation (°)').value).toBe('0');
  });

  it('saves the edited grid and closes', async () => {
    const { onSave, onClose } = open();
    set('Width (px)', '2048');
    set('Rotation (°)', '15');
    fireEvent.click(save());
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(onSave).toHaveBeenCalledWith({ ...GRID, widthPx: 2048, rotationDeg: 15 });
  });

  it('redraws the outlines when the scale changes', () => {
    open();
    const before = outlines().map((o) => o.getAttribute('points'));
    expect(before.every(Boolean)).toBe(true);
    set('Scale (″/px)', '4');
    const after = outlines().map((o) => o.getAttribute('points'));
    expect(after).toHaveLength(3);
    expect(after).not.toEqual(before);
  });

  it('disables Save and says why while a field is outside the bounds', () => {
    const { onSave } = open();
    expect(save()).toBeEnabled();
    set('Width (px)', '63');
    expect(save()).toBeDisabled();
    expect(screen.getByRole('alert')).toBeInTheDocument();
    fireEvent.click(save());
    expect(onSave).not.toHaveBeenCalled();
    set('Width (px)', '64');
    expect(save()).toBeEnabled();
    expect(screen.queryByRole('alert')).toBeNull();
    set('Scale (″/px)', '0');
    expect(save()).toBeDisabled();
    set('Scale (″/px)', '100');
    expect(save()).toBeEnabled();
    set('Centre RA (°)', '360');
    expect(save()).toBeDisabled();
    set('Centre RA (°)', '359.5');
    set('Centre Dec (°)', '-90');
    expect(save()).toBeEnabled();
    set('Centre Dec (°)', '-91');
    expect(save()).toBeDisabled();
  });

  it('shows the field of view of the draft', () => {
    open();
    expect(screen.getByText('2.28° × 1.71°')).toBeInTheDocument();
    set('Width (px)', '1800');
    expect(screen.getByText('1° × 1.71°')).toBeInTheDocument();
  });

  it('keeps itself open on a refused save, shows the message and notifies once', async () => {
    const err = new Error('This account cannot edit the project\'s grid (it needs project.edit and membership).');
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { onClose } = open({ onSave: vi.fn().mockRejectedValue(err) });
    fireEvent.click(save());
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('This account cannot edit the project\'s grid (it needs project.edit and membership).');
    expect(alert.textContent).not.toMatch(/^Error:/);
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock).toHaveBeenCalledWith({
      title: 'Grid not saved', detail: err.message, tone: 'warning', hasErrors: true,
    });
    expect(spy).toHaveBeenCalledWith('[GridDialog] set_collab_project_grid failed:', err);
    // The editor can retry: the buttons are back.
    expect(save()).toBeEnabled();
  });

  it('shows a refusal that arrives as a plain string', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    open({ onSave: vi.fn().mockRejectedValue('hub returned 400 Bad Request (put project grid): grid needs scaleArcsec in (0,100]') });
    fireEvent.click(save());
    expect(await screen.findByRole('alert')).toHaveTextContent('hub returned 400 Bad Request (put project grid)');
    expect(notifyMock).toHaveBeenCalledTimes(1);
  });

  it('clears the refusal when the grid is edited', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    open({ onSave: vi.fn().mockRejectedValue(new Error('refused')) });
    fireEvent.click(save());
    await screen.findByRole('alert');
    set('Width (px)', '2000');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('disables Save, Cancel and the fields while the save is in flight', async () => {
    let finish: () => void = () => {};
    const pending = new Promise<void>((resolve) => { finish = resolve; });
    const { onClose } = open({ onSave: vi.fn().mockReturnValue(pending) });
    fireEvent.click(save());
    expect(save()).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    expect(field('Width (px)')).toBeDisabled();
    await act(async () => { finish(); await pending; });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('shows the rewrite warning above Save only when asked to', () => {
    const { unmount } = open({ warnRewrite: true });
    const warning = screen.getByText(
      "Changing the grid makes every processor's next run rewrite its registered frames. Star matching is kept.",
    );
    expect(warning.compareDocumentPosition(save()) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    unmount();
    open({ warnRewrite: false });
    expect(screen.queryByText(/rewrite its registered frames/)).toBeNull();
  });

  it('closes by Cancel without saving', () => {
    const { onSave, onClose } = open();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onSave).not.toHaveBeenCalled();
  });
});

describe('GridCard', () => {
  const card = (grid: PlanGrid, onSaved = vi.fn()) => ({
    onSaved,
    ...render(withMembers(<GridCard projectId="p1" grid={grid} onSaved={onSaved} />)),
  });

  const PROPOSAL_VALUES = '1.5″/px · 3200 × 2560 px (1.33° × 1.07°) · centre RA 83.82° Dec -5.39° · rotation 0°';
  const GRID_VALUES = '2″/px · 4096 × 3072 px (2.28° × 1.71°) · centre RA 83.8° Dec -5.4° · rotation 0°';

  it('proposes the grid to an editor when none is confirmed', () => {
    card(planGrid('proposed'));
    expect(screen.getByRole('heading', { name: 'Canonical grid — not confirmed' })).toBeInTheDocument();
    expect(screen.getByText(PROPOSAL_VALUES)).toBeInTheDocument();
    expect(screen.getByText('12 accepted frames lie at least half inside')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Use proposal' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Edit…' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Change…' })).toBeNull();
    expect(screen.getByTestId('grid-schematic')).toBeInTheDocument();
  });

  it('counts the accepted frames inside in the singular for one', () => {
    card(planGrid('proposed', { framesInside: 1 }));
    expect(screen.getByText('1 accepted frame lies at least half inside')).toBeInTheDocument();
    expect(screen.queryByText(/1 accepted frames/)).toBeNull();
  });

  it('reads the same for an unset state that carries a proposal', () => {
    card(planGrid('unset'));
    expect(screen.getByRole('heading', { name: 'Canonical grid — not confirmed' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Use proposal' })).toBeInTheDocument();
  });

  it('offers only Edit… with no proposal, prefilled from the project target', () => {
    card(planGrid('unset', { proposal: null, framesInside: 0 }));
    expect(screen.getByText('No proposal yet.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Use proposal' })).toBeNull();
    expect(screen.queryByText(/accepted frames lie at least half inside/)).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Edit…' }));
    const field = (label: string) => (screen.getByLabelText(label) as HTMLInputElement).value;
    expect(field('Scale (″/px)')).toBe('1');
    expect(field('Width (px)')).toBe('4096');
    expect(field('Height (px)')).toBe('4096');
    expect(field('Rotation (°)')).toBe('0');
    expect(field('Centre RA (°)')).toBe('83.82');
    expect(field('Centre Dec (°)')).toBe('-5.39');
    // Still a schematic with no grid: the target and the outlines.
    expect(screen.getAllByTestId('grid-outline').length).toBeGreaterThan(0);
  });

  it('prefills Edit… with the proposal', () => {
    card(planGrid('proposed'));
    fireEvent.click(screen.getByRole('button', { name: 'Edit…' }));
    const dialog = within(screen.getByRole('dialog'));
    expect((dialog.getByLabelText('Scale (″/px)') as HTMLInputElement).value).toBe('1.5');
    expect((dialog.getByLabelText('Width (px)') as HTMLInputElement).value).toBe('3200');
    expect(screen.queryByText(/rewrite its registered frames/)).toBeNull();
  });

  it('only waits for the coordinator when the viewer cannot edit', () => {
    card(planGrid('proposed', { canEdit: false }));
    expect(screen.getByRole('heading', { name: 'Canonical grid — not confirmed' })).toBeInTheDocument();
    expect(screen.getByText('Waiting for the coordinator to confirm the grid')).toBeInTheDocument();
    expect(screen.getByText(PROPOSAL_VALUES)).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('shows the confirmed grid with Change… for an editor', () => {
    card(planGrid('confirmed'));
    expect(screen.getByRole('heading', { name: 'Canonical grid' })).toBeInTheDocument();
    expect(screen.getByText(GRID_VALUES)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Change…' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Use proposal' })).toBeNull();
    expect(screen.queryByText(/not confirmed/)).toBeNull();
  });

  it('shows the confirmed grid read-only to a viewer who cannot edit', () => {
    card(planGrid('confirmed', { canEdit: false }));
    expect(screen.getByText(GRID_VALUES)).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.queryByText(/Waiting for the coordinator/)).toBeNull();
  });

  it('warns about the rewrite only in the dialog opened from Change…', () => {
    card(planGrid('confirmed'));
    fireEvent.click(screen.getByRole('button', { name: 'Change…' }));
    const dialog = within(screen.getByRole('dialog'));
    expect(
      dialog.getByText("Changing the grid makes every processor's next run rewrite its registered frames. Star matching is kept."),
    ).toBeInTheDocument();
    expect((dialog.getByLabelText('Width (px)') as HTMLInputElement).value).toBe('4096');
  });

  it('warns about the rewrite in an Edit… dialog once the grid behind it was confirmed by someone else', () => {
    const view = render(withMembers(<GridCard projectId="p1" grid={planGrid('proposed')} onSaved={vi.fn()} />));
    fireEvent.click(screen.getByRole('button', { name: 'Edit…' }));
    expect(screen.queryByText(/rewrite its registered frames/)).toBeNull();
    // Another member confirmed a grid while the dialog was open; the editor still can edit.
    view.rerender(withMembers(<GridCard projectId="p1" grid={planGrid('confirmed')} onSaved={vi.fn()} />));
    expect(
      within(screen.getByRole('dialog')).getByText(
        "Changing the grid makes every processor's next run rewrite its registered frames. Star matching is kept.",
      ),
    ).toBeInTheDocument();
  });

  it('draws the confirmed grid, not the proposal, in the card', () => {
    card(planGrid('confirmed'));
    expect(Number(screen.getByTestId('grid-rect').getAttribute('width'))).toBe(4096);
  });

  it('uses the proposal as it is', async () => {
    const { onSaved } = card(planGrid('proposed'));
    fireEvent.click(screen.getByRole('button', { name: 'Use proposal' }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));
    expect(api.invoke).toHaveBeenCalledTimes(1);
    expect(api.invoke).toHaveBeenCalledWith('set_collab_project_grid', { projectId: 'p1', grid: PROPOSAL });
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it('disables both buttons while Use proposal is in flight', async () => {
    let finish: (v: unknown) => void = () => {};
    vi.mocked(api.invoke).mockReturnValue(new Promise((resolve) => { finish = resolve; }) as never);
    const { onSaved } = card(planGrid('proposed'));
    fireEvent.click(screen.getByRole('button', { name: 'Use proposal' }));
    expect(screen.getByRole('button', { name: 'Use proposal' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Edit…' })).toBeDisabled();
    await act(async () => { finish(PROPOSAL); await Promise.resolve(); });
    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));
    expect(screen.getByRole('button', { name: 'Use proposal' })).toBeEnabled();
  });

  it('saves the edited grid from the dialog, closes it and tells the tab', async () => {
    const { onSaved } = card(planGrid('confirmed'));
    fireEvent.click(screen.getByRole('button', { name: 'Change…' }));
    fireEvent.change(screen.getByLabelText('Rotation (°)'), { target: { value: '10' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(api.invoke).toHaveBeenCalledWith('set_collab_project_grid', { projectId: 'p1', grid: { ...GRID, rotationDeg: 10 } });
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it('keeps a refused Use proposal visible, notifies once and does not refetch', async () => {
    const err = 'This account cannot edit the project\'s grid (it needs project.edit and membership).';
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue(err);
    const { onSaved } = card(planGrid('proposed'));
    fireEvent.click(screen.getByRole('button', { name: 'Use proposal' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(err);
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock).toHaveBeenCalledWith({ title: 'Grid not saved', detail: err, tone: 'warning', hasErrors: true });
    expect(spy).toHaveBeenCalledWith('[GridCard] set_collab_project_grid failed:', err);
    expect(onSaved).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Use proposal' })).toBeEnabled();
  });

  it('clears the refusal of Use proposal after a successful save from Edit…', async () => {
    const err = 'hub returned 400 Bad Request (put project grid): out of bounds';
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(err);
    const { onSaved } = card(planGrid('proposed'));
    fireEvent.click(screen.getByRole('button', { name: 'Use proposal' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(err);

    vi.mocked(api.invoke).mockResolvedValue(undefined as never);
    fireEvent.click(screen.getByRole('button', { name: 'Edit…' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(onSaved).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('clears the refusal of Use proposal when the shown grid becomes confirmed', async () => {
    const err = 'hub returned 403 Forbidden (put project grid): no project.edit';
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue(err);
    const view = render(withMembers(<GridCard projectId="p1" grid={planGrid('proposed')} onSaved={vi.fn()} />));
    fireEvent.click(screen.getByRole('button', { name: 'Use proposal' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(err);
    view.rerender(withMembers(<GridCard projectId="p1" grid={planGrid('confirmed')} onSaved={vi.fn()} />));
    expect(screen.queryByRole('alert')).toBeNull();
  });

  // Review Focus 4: a save the hub refused (403 or 400) or another member got in first. The dialog stays open with
  // the text, `notify` fires once, no refetch is asked for by the failure itself; the next `collab-grid-changed`
  // brings the plan again, and the card behind the dialog then shows the grid the other member confirmed.
  it('keeps the dialog open on a refused save and shows the newer grid behind it once the plan comes again', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue(new Error('hub returned 403 Forbidden (put project grid): no project.edit'));
    const onSaved = vi.fn();
    const view = render(withMembers(<GridCard projectId="p1" grid={planGrid('proposed')} onSaved={onSaved} />));
    fireEvent.click(screen.getByRole('button', { name: 'Edit…' }));
    fireEvent.change(screen.getByLabelText('Width (px)'), { target: { value: '2048' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('hub returned 403 Forbidden (put project grid): no project.edit');
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(onSaved).not.toHaveBeenCalled();

    // The refetched plan: another member confirmed a grid first.
    view.rerender(withMembers(<GridCard projectId="p1" grid={planGrid('confirmed', { canEdit: false })} onSaved={onSaved} />));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent('no project.edit');
    expect(screen.getAllByText('Canonical grid').length).toBeGreaterThan(0);
    expect(notifyMock).toHaveBeenCalledTimes(1);
  });
});
