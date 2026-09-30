import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import FrameDrawer from './FrameDrawer';
import { api } from '../../../api';
import type { FrameHolderView, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import type { FrameVM } from './frames';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

afterEach(cleanup);

function baseOwn(overrides: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1,
    frameUuid: 'uuid-1',
    fileName: 'light_001.fits',
    setId: 7,
    setName: 'M31',
    night: '2026-09-20',
    filter: 'L',
    filterMapped: true,
    camera: 'ASI2600MM',
    exptimeSec: 300,
    byteSize: 42_000_000,
    fwhmArcsec: 3.42,
    eccentricity: 0.4,
    starsDetected: 500,
    medianSnr: 12,
    segment: 'held',
    contributorState: 'failsGate',
    contributorReason: null,
    failures: [],
    contentVersion: null,
    pubState: null,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    localState: 'own_held',
    publishedAt: null,
    lastError: null,
    rules: [],
    path: '/Volumes/Astro/M31/2026-09-20/light_001.fits',
    accepted: null,
    ...overrides,
  };
}

function baseLib(overrides: Partial<ProjectFrameView> = {}): ProjectFrameView {
  return {
    frameUuid: 'uuid-1',
    fileName: 'light_001.fits',
    publisher: 'Olga',
    publisherAccountId: 'acc-olga',
    own: false,
    filter: 'L',
    exptimeSec: 300,
    dateObs: null,
    state: 'published',
    accepted: true,
    acceptedReason: null,
    localState: 'held',
    onDisk: true,
    holdersOnline: 1,
    holdersTotal: 2,
    waitingForPublisher: false,
    newVersionWaiting: false,
    byteSize: 1000,
    contentVersion: 1,
    lastError: null,
    fwhmArcsec: null,
    eccentricity: null,
    starsDetected: null,
    camera: null,
    telescope: null,
    medianSnr: null,
    night: null,
    contributorState: null,
    contributorReason: null,
    receivedAt: null,
    receivedFromDevice: null,
    receivedFromMember: null,
    ...overrides,
  };
}

function baseFrame(overrides: Partial<FrameVM> = {}): FrameVM {
  return {
    key: 'uuid-1',
    frameId: 1,
    frameUuid: 'uuid-1',
    hasProjectRow: true,
    setId: null,
    setName: null,
    fileName: 'light_001.fits',
    night: '2026-09-20',
    filter: 'L',
    filterMapped: true,
    camera: 'ASI2600MM',
    publisher: null,
    publisherAccountId: null,
    exptimeSec: 300,
    byteSize: 42_000_000,
    fwhm: 3.42,
    ecc: 0.4,
    stars: 500,
    snr: 12,
    failures: [],
    contentVersion: null,
    pubState: null,
    excluded: false,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    disk: null,
    publishedAt: null,
    device: null,
    missingWhy: null,
    progress: null,
    submittedAt: null,
    states: [],
    own: null,
    lib: null,
    mod: null,
    ...overrides,
  };
}

function renderDrawer(
  frame: FrameVM,
  props: Partial<{ canModerate: boolean; onClose: () => void; onChanged: () => void }> = {},
) {
  return render(
    <MemoryRouter>
      <FrameDrawer
        projectId="p"
        frame={frame}
        canModerate={props.canModerate ?? false}
        onClose={props.onClose ?? vi.fn()}
        onChanged={props.onChanged ?? vi.fn()}
      />
    </MemoryRouter>,
  );
}

/** Every `api.listen` registration a render made, by event name. `fire`
 *  delivers a payload to every listener registered for that event. */
const listeners: Record<string, ((payload: unknown) => void)[]> = {};

function fire(event: string, payload: unknown) {
  act(() => {
    for (const cb of listeners[event] ?? []) cb(payload);
  });
}

beforeEach(() => {
  for (const k of Object.keys(listeners)) delete listeners[k];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_frame_holders') return Promise.resolve([] as FrameHolderView[]);
    return Promise.reject(new Error(`unexpected ${command}`));
  }) as typeof api.invoke);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    (listeners[event] ??= []).push(cb as unknown as (payload: unknown) => void);
    return Promise.resolve(() => {});
  }) as never);
});

describe('FrameDrawer', () => {
  it('(a) lists the precondition failure and the rule verdicts for an own held frame', async () => {
    const own = baseOwn({
      failures: [{ kind: 'solve', text: 'unknown pixel scale' }],
      rules: [
        { metricKey: 'fwhm', label: 'FWHM', value: '3.42″', needs: '≤ 3.00″', pass: false },
        { metricKey: 'stars', label: 'Stars', value: '500', needs: '≥ 200', pass: true },
      ],
    });
    renderDrawer(baseFrame({ own, failures: own.failures }));

    expect(screen.getByText('✕ unknown pixel scale')).toBeInTheDocument();

    const table = screen.getByRole('table');
    const fwhmRow = within(table).getByText('FWHM').closest('tr')!;
    expect(within(fwhmRow).getByText('3.42″')).toBeInTheDocument();
    expect(within(fwhmRow).getByText('≤ 3.00″')).toBeInTheDocument();
    expect(within(fwhmRow).getByText('✕')).toBeInTheDocument();

    const starsRow = within(table).getByText('Stars').closest('tr')!;
    expect(within(starsRow).getByText('500')).toBeInTheDocument();
    expect(within(starsRow).getByText('≥ 200')).toBeInTheDocument();
    expect(within(starsRow).getByText('✓')).toBeInTheDocument();

    await waitFor(() => expect(screen.getByText('Nobody else holds it yet.')).toBeInTheDocument());
  });

  it('(b) shows — for a rule that was not evaluated', () => {
    const own = baseOwn({
      rules: [{ metricKey: 'ecc', label: 'Eccentricity', value: null, needs: '≤ 0.55', pass: null }],
    });
    renderDrawer(baseFrame({ own }));

    const table = screen.getByRole('table');
    const row = within(table).getByText('Eccentricity').closest('tr')!;
    expect(within(row).getByTitle('not evaluated — see above')).toBeInTheDocument();
    expect(within(row).getByTitle('not evaluated — see above')).toHaveTextContent('—');
  });

  it('(c) shows the own frame\'s local path', () => {
    const own = baseOwn({ path: '/Volumes/Astro/M31/2026-09-20/light_001.fits' });
    renderDrawer(baseFrame({ own }));
    expect(screen.getByText('/Volumes/Astro/M31/2026-09-20/light_001.fits')).toBeInTheDocument();
  });

  it('(d) holders load and render name/device/publisher chip; unknown member shows short id', async () => {
    const holders: FrameHolderView[] = [
      {
        memberName: 'Kostya',
        deviceName: 'kostya-obs',
        device: 'abcdef0123456789',
        deviceShort: 'abcdef01',
        online: true,
        isPublisher: true,
        contentVersion: 2,
      },
      {
        memberName: null,
        deviceName: null,
        device: 'deadbeefcafebabe',
        deviceShort: 'deadbeef',
        online: false,
        isPublisher: false,
        contentVersion: 1,
      },
    ];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return Promise.resolve(holders);
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);

    renderDrawer(baseFrame());

    await waitFor(() => expect(screen.getByText('Kostya')).toBeInTheDocument());
    expect(screen.getByText('kostya-obs')).toBeInTheDocument();
    expect(screen.getByText('publisher')).toBeInTheDocument();
    expect(screen.getByText('Unknown member')).toBeInTheDocument();
    expect(screen.getByText('deadbeef')).toBeInTheDocument();
  });

  it('(e) a failed holders call shows an inline error', async () => {
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return Promise.reject(new Error('offline'));
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);

    renderDrawer(baseFrame());

    await waitFor(() => expect(screen.getByText('Could not load holders.')).toBeInTheDocument());
    expect(errSpy).toHaveBeenCalledWith('[drawer] holders failed:', expect.any(Error));
    errSpy.mockRestore();
  });

  it('(e2) an own frame not yet published (uuid, no project row) asks for no holders and shows no holders section', () => {
    renderDrawer(baseFrame({ hasProjectRow: false, pubState: null }));

    expect(api.invoke).not.toHaveBeenCalledWith('get_collab_frame_holders', expect.anything());
    expect(screen.queryByText('Who holds it')).toBeNull();
  });

  it('(f) Escape calls onClose', () => {
    const onClose = vi.fn();
    renderDrawer(baseFrame(), { onClose });
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalled();
  });

  it('fix round 1: Escape closes only the ExcludeDialog when it is open, not the drawer underneath', () => {
    const onClose = vi.fn();
    const frame = baseFrame({ excluded: false, pubState: 'published' });
    renderDrawer(frame, { canModerate: true, onClose });

    fireEvent.click(screen.getByRole('button', { name: 'Exclude…' }));
    expect(screen.getByText('Exclude 1 frame from the project')).toBeInTheDocument();

    fireEvent.keyDown(document, { key: 'Escape' });

    expect(screen.queryByText('Exclude 1 frame from the project')).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('(g) shows the provenance line for a received library frame', () => {
    const lib = baseLib({
      receivedAt: '2026-09-20T10:00:00Z',
      receivedFromDevice: 'somehexdeviceid',
      receivedFromMember: 'Olga',
    });
    renderDrawer(baseFrame({ lib }));
    expect(screen.getByText(/Received .* from Olga/)).toBeInTheDocument();
  });

  it('(g2) falls back to the device\'s first 8 chars, or "this device\'s files" for local', () => {
    const localLib = baseLib({
      receivedAt: '2026-09-20T10:00:00Z',
      receivedFromDevice: 'local',
      receivedFromMember: null,
    });
    const { unmount } = renderDrawer(baseFrame({ lib: localLib }));
    expect(screen.getByText(/Received .* from this device's files/)).toBeInTheDocument();
    unmount();

    const deviceLib = baseLib({
      receivedAt: '2026-09-20T10:00:00Z',
      receivedFromDevice: 'abcdef0123456789',
      receivedFromMember: null,
    });
    renderDrawer(baseFrame({ lib: deviceLib }));
    expect(screen.getByText(/Received .* from abcdef01/)).toBeInTheDocument();
  });

  it('(h) canModerate sees Restore on an excluded frame, which restores and calls onChanged; without canModerate there is no button', async () => {
    const onChanged = vi.fn();
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return Promise.resolve([]);
      if (command === 'restore_collab_frame') return Promise.resolve(undefined);
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);
    const frame = baseFrame({ excluded: true, acceptedReason: 'trailed', pubState: 'published' });

    const { unmount } = renderDrawer(frame, { canModerate: true, onChanged });
    expect(screen.getByText(/Excluded — trailed/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Restore' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('restore_collab_frame', { projectId: 'p', frameUuid: 'uuid-1' }),
    );
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    unmount();

    renderDrawer(frame, { canModerate: false });
    expect(screen.getByText(/Excluded — trailed/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Restore' })).not.toBeInTheDocument();
  });

  it('(i) collab-peers-changed for this project re-reads holders and swaps in the new answer without flashing "Loading holders…"', async () => {
    const first: FrameHolderView[] = [
      {
        memberName: 'Kostya',
        deviceName: 'kostya-obs',
        device: 'abcdef0123456789',
        deviceShort: 'abcdef01',
        online: true,
        isPublisher: true,
        contentVersion: 1,
      },
    ];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return Promise.resolve(first);
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);

    renderDrawer(baseFrame());
    await waitFor(() => expect(screen.getByText('Kostya')).toBeInTheDocument());

    let resolveSecond!: (rows: FrameHolderView[]) => void;
    const second = new Promise<FrameHolderView[]>((resolve) => {
      resolveSecond = resolve;
    });
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return second;
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);

    fire('collab-peers-changed', { projectId: 'p' });

    // The re-read is in flight — the OLD holder stays, no loading flash.
    expect(screen.getByText('Kostya')).toBeInTheDocument();
    expect(screen.queryByText('Loading holders…')).not.toBeInTheDocument();

    await act(async () => {
      resolveSecond([
        {
          memberName: 'Olga',
          deviceName: 'olga-mac',
          device: 'deadbeefcafebabe',
          deviceShort: 'deadbeef',
          online: true,
          isPublisher: false,
          contentVersion: 2,
        },
      ]);
      await Promise.resolve();
    });

    await waitFor(() => expect(screen.getByText('Olga')).toBeInTheDocument());
    expect(screen.queryByText('Kostya')).not.toBeInTheDocument();
    expect(api.invoke).toHaveBeenCalledWith('get_collab_frame_holders', {
      projectId: 'p',
      frameUuid: 'uuid-1',
    });
  });

  it('(j) collab-peers-changed for another project re-reads nothing', async () => {
    const first: FrameHolderView[] = [
      {
        memberName: 'Kostya',
        deviceName: 'kostya-obs',
        device: 'abcdef0123456789',
        deviceShort: 'abcdef01',
        online: true,
        isPublisher: true,
        contentVersion: 1,
      },
    ];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return Promise.resolve(first);
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);
    renderDrawer(baseFrame());
    await waitFor(() => expect(screen.getByText('Kostya')).toBeInTheDocument());
    const before = vi
      .mocked(api.invoke)
      .mock.calls.filter(([c]) => c === 'get_collab_frame_holders').length;

    fire('collab-peers-changed', { projectId: 'another-project' });
    await act(async () => {
      await Promise.resolve();
    });

    const after = vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_collab_frame_holders').length;
    expect(after).toBe(before);
  });
});
