import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import FramePanel from './FramePanel';
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
    calibratedPath: null,
    calibratedBytes: null,
    preparedAt: null,
    withheld: false,
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

function renderPanel(
  frame: FrameVM,
  props: Partial<{ canModerate: boolean; onClose: () => void; onChanged: () => void; thresholdsVersion: number | null }> = {},
) {
  return render(
    <MemoryRouter>
      <FramePanel
        projectId="p"
        frame={frame}
        canModerate={props.canModerate ?? false}
        onClose={props.onClose ?? vi.fn()}
        onChanged={props.onChanged ?? vi.fn()}
        thresholdsVersion={props.thresholdsVersion ?? null}
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

describe('FramePanel', () => {
  it('(a) lists the precondition failure and the rule verdicts for an own held frame', async () => {
    const own = baseOwn({
      failures: [{ kind: 'solve', text: 'unknown pixel scale' }],
      rules: [
        { metricKey: 'fwhm', label: 'FWHM', value: '3.42″', needs: '≤ 3.00″', pass: false },
        { metricKey: 'stars', label: 'Stars', value: '500', needs: '≥ 200', pass: true },
      ],
    });
    renderPanel(baseFrame({ own, failures: own.failures }));

    expect(screen.getByText('Plate-solved')).toBeInTheDocument();
    const fwhmLabel = screen.getByText('FWHM', { selector: 'span' });
    expect(fwhmLabel).toBeInTheDocument();
    expect(screen.getByText('3.42″', { selector: 'span' })).toBeInTheDocument();
    expect(screen.getByText('≤ 3.00″')).toBeInTheDocument();
    expect(screen.getByText('≥ 200')).toBeInTheDocument();
    // Stars rule + Analyzed + Filter mapped pass; FWHM rule + Plate-solved fail.
    expect(screen.getAllByText('✓')).toHaveLength(3);
    expect(screen.getAllByText('✕')).toHaveLength(2);

    await waitFor(() => expect(screen.getByText('Nobody else holds it yet.')).toBeInTheDocument());
  });

  it('(b) shows a dash and no verdict for a rule that was not evaluated', () => {
    const own = baseOwn({
      rules: [{ metricKey: 'ecc', label: 'Eccentricity', value: null, needs: '≤ 0.55', pass: null }],
    });
    renderPanel(baseFrame({ own }));

    expect(screen.getByText('Eccentricity', { selector: 'span' })).toBeInTheDocument();
    expect(screen.getByText('≤ 0.55')).toBeInTheDocument();
    // Three precondition rows pass; the un-evaluated rule adds no verdict.
    expect(screen.getAllByText('✓')).toHaveLength(3);
  });

  it('(c) shows the own frame\'s local path', () => {
    const own = baseOwn({ path: '/Volumes/Astro/M31/2026-09-20/light_001.fits' });
    renderPanel(baseFrame({ own }));
    expect(screen.getByText('/Volumes/Astro/M31/2026-09-20/light_001.fits')).toBeInTheDocument();
  });

  it('(c2) a To review frame shows the "to review" chip and its Calibrated path with a copy button', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const own = baseOwn({ segment: 'review', calibratedPath: '/Collab/prepared/c_light_001.fits', calibratedBytes: 84_000_000 });
    renderPanel(baseFrame({ own }));
    expect(screen.getByText('to review')).toBeInTheDocument();
    const line = screen.getByText('/Collab/prepared/c_light_001.fits').parentElement!;
    expect(line).toHaveTextContent('Calibrated');
    fireEvent.click(screen.getByRole('button', { name: 'Copy calibrated path' }));
    expect(writeText).toHaveBeenCalledWith('/Collab/prepared/c_light_001.fits');
    // The raw path keeps its own copy button.
    fireEvent.click(screen.getByRole('button', { name: 'Copy path' }));
    expect(writeText).toHaveBeenLastCalledWith('/Volumes/Astro/M31/2026-09-20/light_001.fits');
    await waitFor(() => expect(api.invoke).toHaveBeenCalled());
    Reflect.deleteProperty(navigator, 'clipboard');
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

    renderPanel(baseFrame());

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

    renderPanel(baseFrame());

    await waitFor(() => expect(screen.getByText('Could not load holders — see console.')).toBeInTheDocument());
    expect(errSpy).toHaveBeenCalledWith('[drawer] holders failed:', expect.any(Error));
    errSpy.mockRestore();
  });

  it('(e2) an own frame not yet published (uuid, no project row) asks for no holders and shows no holders section', () => {
    renderPanel(baseFrame({ hasProjectRow: false, pubState: null }));

    expect(api.invoke).not.toHaveBeenCalledWith('get_collab_frame_holders', expect.anything());
    expect(screen.queryByText('Who holds it')).toBeNull();
  });

  it('(f) Escape calls onClose', () => {
    const onClose = vi.fn();
    renderPanel(baseFrame(), { onClose });
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalled();
  });

  it('fix round 1: Escape closes only the ExcludeDialog when it is open, not the drawer underneath', () => {
    const onClose = vi.fn();
    const frame = baseFrame({ excluded: false, pubState: 'published' });
    renderPanel(frame, { canModerate: true, onClose });

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
    renderPanel(baseFrame({ lib }));
    expect(screen.getByText(/Received .* from Olga/)).toBeInTheDocument();
  });

  it('(g2) falls back to the device\'s first 8 chars, or "this device\'s files" for local', () => {
    const localLib = baseLib({
      receivedAt: '2026-09-20T10:00:00Z',
      receivedFromDevice: 'local',
      receivedFromMember: null,
    });
    const { unmount } = renderPanel(baseFrame({ lib: localLib }));
    expect(screen.getByText(/Received .* from this device's files/)).toBeInTheDocument();
    unmount();

    const deviceLib = baseLib({
      receivedAt: '2026-09-20T10:00:00Z',
      receivedFromDevice: 'abcdef0123456789',
      receivedFromMember: null,
    });
    renderPanel(baseFrame({ lib: deviceLib }));
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

    const { unmount } = renderPanel(frame, { canModerate: true, onChanged });
    expect(screen.getByText(/Excluded — trailed/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Restore' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('restore_collab_frame', { projectId: 'p', frameUuid: 'uuid-1' }),
    );
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    unmount();

    renderPanel(frame, { canModerate: false });
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

    renderPanel(baseFrame());
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
    expect(screen.queryByText('Loading…')).not.toBeInTheDocument();

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
    renderPanel(baseFrame());
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

  function lib() { return baseLib(); }
  function own(overrides: Partial<OwnFrameRow> = {}) { return baseOwn(overrides); }
  function holdersAnswer(rows: FrameHolderView[]) {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_frame_holders') return Promise.resolve(rows);
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);
  }

  it('title block: mono file name and status chips', () => {
    renderPanel(baseFrame({ own: null, lib: lib(), pubState: 'published', device: 'have' }));
    expect(screen.getByText('light_001.fits').className).toContain('font-mono');
    expect(screen.getByText('published').className).toContain('bg-success-muted');
    expect(screen.getByText('have')).toBeInTheDocument();
  });

  it('Frame section is a KV with the mockup labels', () => {
    renderPanel(baseFrame({ night: '2026-08-31', exptimeSec: 180, byteSize: 121_920_698, contentVersion: 2, publisher: 'Olga' }));
    for (const label of ['Publisher', 'Night', 'Filter', 'Camera', 'Exposure', 'Size', 'Version']) {
      expect(screen.getByText(label).tagName).toBe('DT');
    }
    expect(screen.getByText('2026-08-31 · Mon')).toBeInTheDocument();
    expect(screen.getByText('180 s')).toBeInTheDocument();
    expect(screen.getByText('122 MB')).toBeInTheDocument();
    expect(screen.getByText('v2 · v1 superseded')).toBeInTheDocument();
  });

  it('Metrics: SNR to one decimal, no Zero point row', () => {
    renderPanel(baseFrame({ snr: 23.054622650146484 }));
    expect(screen.getByText('23.1')).toBeInTheDocument();
    expect(screen.queryByText('Zero point')).toBeNull();
  });

  it('Gate is a 4-column grid with the thresholds version and precondition rows', () => {
    renderPanel(
      baseFrame({ own: own({ failures: [{ kind: 'solve', text: 'No WCS' }], rules: [{ metricKey: 'fwhm', label: 'FWHM', value: '3.42″', needs: '≤ 3.00″', pass: false }] }) }),
      { thresholdsVersion: 3 },
    );
    expect(screen.getByRole('heading', { name: /Gate thresholds v3/ })).toBeInTheDocument();
    expect(screen.getByText('Plate-solved')).toBeInTheDocument();
    expect(screen.getByText('3.42″', { selector: 'span' })).toBeInTheDocument();
    expect(screen.getAllByText('✕').length).toBeGreaterThanOrEqual(2);
  });

  it('Who holds it: "N online of M", publisher chip, device on the right', async () => {
    holdersAnswer([
      { memberName: 'Andrei', deviceName: 'andrei-pc', device: 'd1', deviceShort: 'd1', online: true, isPublisher: true, contentVersion: 1 },
      { memberName: 'Olga', deviceName: 'olga-home', device: 'd2', deviceShort: 'd2', online: false, isPublisher: false, contentVersion: 1 },
    ]);
    renderPanel(baseFrame({ lib: lib() }));
    expect(await screen.findByRole('heading', { name: /Who holds it 1 online of 2/ })).toBeInTheDocument();
    expect(screen.getByText('publisher')).toBeInTheDocument();
    expect(screen.getByText('andrei-pc').className).toContain('ml-auto');
  });

  it('Gate lists each blocker text with a failing row', () => {
    renderPanel(baseFrame({ own: own({ failures: [
      { kind: 'linkCalibration', text: 'No master flat for B, bin 1' },
      { kind: 'attest', text: 'Not calibrated' },
    ] }) }));
    expect(screen.getByText('No master flat for B, bin 1')).toBeInTheDocument();
    expect(screen.getAllByText('Not calibrated').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('✕')).toHaveLength(2);
  });

  it('a failing precondition shows its failure text in the Value cell', () => {
    renderPanel(baseFrame({ own: own({ failures: [{ kind: 'solve', text: 'unknown pixel scale' }] }) }));
    expect(screen.getByText('unknown pixel scale')).toBeInTheDocument();
    expect(screen.getAllByText('✕')).toHaveLength(1);
  });

  it('a long holder device name truncates and carries a title', async () => {
    const long = 'd'.repeat(40);
    holdersAnswer([{ memberName: 'Andrei', deviceName: long, device: 'd1', deviceShort: 'd1', online: true, isPublisher: false, contentVersion: 1 }]);
    renderPanel(baseFrame({ lib: lib(), contentVersion: 1 }));
    const el = await screen.findByText(long);
    expect(el.className).toContain('truncate');
    expect(el).toHaveAttribute('title', long);
  });

  it('no version chip on holders when the frame has no content version', async () => {
    holdersAnswer([{ memberName: 'Andrei', deviceName: 'pc', device: 'd1', deviceShort: 'd1', online: true, isPublisher: false, contentVersion: 2 }]);
    renderPanel(baseFrame({ contentVersion: null }));
    await screen.findByText('Andrei');
    expect(screen.queryByText('v2')).toBeNull();
  });

  it('a moderation frame shows only the status chip in the title row', () => {
    renderPanel(baseFrame({ own: null, lib: null, device: null, pubState: 'pending' }));
    expect(screen.getByText('pending')).toBeInTheDocument();
    expect(screen.queryByText('—')).toBeNull();
  });
});
