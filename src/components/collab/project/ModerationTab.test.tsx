import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { SessionStateProvider } from '../../../contexts/SessionStateContext';
import { api } from '../../../api';
import ModerationTab from './ModerationTab';
import type { ModerationFrameView, ProjectFrameView } from '../../../types/models';
import type { FrameVM } from './frames';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(cleanup);

function mod(overrides: Partial<ModerationFrameView> = {}): ModerationFrameView {
  return {
    frameUuid: 'u-1',
    fileName: 'light_001.fits',
    publisher: 'Alice',
    publisherAccountId: 'acc-1',
    filter: 'L',
    exptimeSec: 120,
    fwhmArcsec: 2.1,
    createdAt: '2026-09-29 10:00:00',
    ...overrides,
  };
}

function libraryFrame(overrides: Partial<ProjectFrameView> = {}): ProjectFrameView {
  return {
    frameUuid: 'u-1',
    fileName: 'light_001.fits',
    publisher: 'Alice',
    publisherAccountId: 'acc-1',
    own: false,
    filter: 'L',
    exptimeSec: 120,
    dateObs: null,
    state: 'pending',
    accepted: true,
    acceptedReason: null,
    localState: 'held',
    onDisk: true,
    holdersOnline: 1,
    holdersTotal: 1,
    waitingForPublisher: false,
    newVersionWaiting: false,
    byteSize: 1024,
    contentVersion: 1,
    lastError: null,
    fwhmArcsec: 2.1,
    eccentricity: 0.3,
    starsDetected: 500,
    camera: 'ASI2600MM Pro',
    telescope: null,
    night: '2026-09-28',
    medianSnr: 15,
    contributorState: null,
    contributorReason: null,
    receivedAt: null,
    receivedFromDevice: null,
    receivedFromMember: null,
    ...overrides,
  };
}

let moderationItems: ModerationFrameView[] = [];

beforeEach(() => {
  moderationItems = [];
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'list_collab_moderation') return Promise.resolve(moderationItems);
    return Promise.resolve(null);
  }) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

function renderTab(
  library: ProjectFrameView[] | null = null,
  overrides: { onDecided?: () => void; onOpen?: (vm: FrameVM) => void; requireApproval?: boolean } = {},
) {
  const onDecided = overrides.onDecided ?? vi.fn();
  const onOpen = overrides.onOpen ?? vi.fn();
  const requireApproval = overrides.requireApproval ?? true;
  const utils = render(
    <SessionStateProvider>
      <ModerationTab
        projectId="proj-1"
        requireApproval={requireApproval}
        library={library}
        onDecided={onDecided}
        onOpen={onOpen}
      />
    </SessionStateProvider>,
  );
  return { onDecided, onOpen, ...utils };
}

function approveCalls(): { projectId: string; frameUuid: string; trust: boolean }[] {
  return vi
    .mocked(api.invoke)
    .mock.calls.filter(([cmd]) => cmd === 'approve_collab_frame')
    .map(([, args]) => args as { projectId: string; frameUuid: string; trust: boolean });
}

function rejectCalls(): { projectId: string; frameUuid: string; reason: string }[] {
  return vi
    .mocked(api.invoke)
    .mock.calls.filter(([cmd]) => cmd === 'reject_collab_frame')
    .map(([, args]) => args as { projectId: string; frameUuid: string; reason: string });
}

function restoreCalls(): { projectId: string; frameUuid: string }[] {
  return vi
    .mocked(api.invoke)
    .mock.calls.filter(([cmd]) => cmd === 'restore_collab_frame')
    .map(([, args]) => args as { projectId: string; frameUuid: string });
}

describe('ModerationTab — empty and load', () => {
  it('shows the empty text when nothing is pending', async () => {
    renderTab();
    expect(await screen.findByText('Nothing waiting for review.')).toBeInTheDocument();
  });

  it('shows a load error inline', async () => {
    vi.mocked(api.invoke).mockImplementation((() => Promise.reject(new Error('offline'))) as never);
    renderTab();
    expect(await screen.findByText('offline')).toBeInTheDocument();
  });
});

describe('ModerationTab — Approve', () => {
  it('sends one approve_collab_frame per pending frame with trust: true (default on)', async () => {
    moderationItems = [
      mod({ frameUuid: 'u1', fileName: 'a.fits' }),
      mod({ frameUuid: 'u2', fileName: 'b.fits' }),
    ];
    renderTab();
    const btn = await screen.findByRole('button', { name: 'Approve all 2' });
    fireEvent.click(btn);

    await waitFor(() => expect(approveCalls()).toHaveLength(2));
    expect(approveCalls()).toEqual([
      { projectId: 'proj-1', frameUuid: 'u1', trust: true },
      { projectId: 'proj-1', frameUuid: 'u2', trust: true },
    ]);
  });

  it('unchecking "Trust these publishers" sends trust: false', async () => {
    moderationItems = [mod({ frameUuid: 'u1', fileName: 'a.fits' })];
    renderTab();
    await screen.findByText('a.fits');

    fireEvent.click(screen.getByRole('checkbox', { name: 'Trust these publishers' }));
    fireEvent.click(screen.getByRole('button', { name: 'Approve all 1' }));

    await waitFor(() => expect(approveCalls()).toHaveLength(1));
    expect(approveCalls()[0]).toEqual({ projectId: 'proj-1', frameUuid: 'u1', trust: false });
  });

  it('a failure on the second of three stops the batch and reads "Approved 1 of 3"', async () => {
    moderationItems = [
      mod({ frameUuid: 'u1', fileName: 'a.fits' }),
      mod({ frameUuid: 'u2', fileName: 'b.fits' }),
      mod({ frameUuid: 'u3', fileName: 'c.fits' }),
    ];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'list_collab_moderation') return Promise.resolve(moderationItems);
      if (command === 'approve_collab_frame') {
        // `mock.calls` already records the in-flight call itself (vi.fn
        // appends synchronously before this implementation runs), so the
        // second invocation sees length 2, not 1.
        const n = approveCalls().length;
        if (n === 2) return Promise.reject(new Error('boom'));
        return Promise.resolve(null);
      }
      return Promise.resolve(null);
    }) as never);

    const { onDecided } = renderTab();
    const btn = await screen.findByRole('button', { name: 'Approve all 3' });
    fireEvent.click(btn);

    expect(await screen.findByText(/Approved 1 of 3 — boom/)).toBeInTheDocument();
    expect(approveCalls()).toHaveLength(2);
    expect(approveCalls().map((c) => c.frameUuid)).toEqual(['u1', 'u2']);
    expect(onDecided).toHaveBeenCalledTimes(1);
  });

  it('an "already decided" error (trust cascade or another moderator) is benign and the batch continues', async () => {
    moderationItems = [
      mod({ frameUuid: 'u1', fileName: 'a.fits' }),
      mod({ frameUuid: 'u2', fileName: 'b.fits' }),
      mod({ frameUuid: 'u3', fileName: 'c.fits' }),
    ];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'list_collab_moderation') return Promise.resolve(moderationItems);
      if (command === 'approve_collab_frame') {
        const n = approveCalls().length;
        // The first frame's own approve succeeds; trust:true's cascade on the
        // hub has already decided the other two by the time this batch
        // reaches them.
        if (n >= 2) return Promise.reject(new Error('This frame was already decided — refresh the queue.'));
        return Promise.resolve(null);
      }
      return Promise.resolve(null);
    }) as never);

    const { onDecided } = renderTab();
    const btn = await screen.findByRole('button', { name: 'Approve all 3' });
    fireEvent.click(btn);

    await waitFor(() => expect(approveCalls()).toHaveLength(3));
    expect(approveCalls().map((c) => c.frameUuid)).toEqual(['u1', 'u2', 'u3']);
    await waitFor(() => expect(onDecided).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(/^Approved/)).not.toBeInTheDocument();
  });
});

describe('ModerationTab — Reject', () => {
  it('requires a reason and sends it for every selected frame', async () => {
    moderationItems = [
      mod({ frameUuid: 'u1', fileName: 'a.fits' }),
      mod({ frameUuid: 'u2', fileName: 'b.fits' }),
    ];
    renderTab();
    await screen.findByText('a.fits');

    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Reject 2' }));

    expect(screen.getByText('Reject 2 frames')).toBeInTheDocument();
    const submit = screen.getByRole('button', { name: 'Reject' });
    expect(submit).toBeDisabled();

    fireEvent.change(screen.getByPlaceholderText('Why is this frame rejected?'), {
      target: { value: 'blurry' },
    });
    expect(submit).not.toBeDisabled();
    fireEvent.click(submit);

    await waitFor(() => expect(rejectCalls()).toHaveLength(2));
    expect(rejectCalls()).toEqual([
      { projectId: 'proj-1', frameUuid: 'u1', reason: 'blurry' },
      { projectId: 'proj-1', frameUuid: 'u2', reason: 'blurry' },
    ]);
  });

  it('an "already decided" error is benign and the reject batch continues', async () => {
    moderationItems = [
      mod({ frameUuid: 'u1', fileName: 'a.fits' }),
      mod({ frameUuid: 'u2', fileName: 'b.fits' }),
    ];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'list_collab_moderation') return Promise.resolve(moderationItems);
      if (command === 'reject_collab_frame') {
        if (rejectCalls().length >= 1) {
          return Promise.reject(new Error('This frame was already decided — refresh the queue.'));
        }
        return Promise.resolve(null);
      }
      return Promise.resolve(null);
    }) as never);

    const { onDecided } = renderTab();
    await screen.findByText('a.fits');
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Reject 2' }));
    fireEvent.change(screen.getByPlaceholderText('Why is this frame rejected?'), {
      target: { value: 'blurry' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Reject' }));

    await waitFor(() => expect(rejectCalls()).toHaveLength(2));
    await waitFor(() => expect(onDecided).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(/^Rejected/)).not.toBeInTheDocument();
  });
});

describe('ModerationTab — manifest mirror', () => {
  it('shows night from the library mirror when the frame has already landed', async () => {
    moderationItems = [mod({ frameUuid: 'u-1', fileName: 'landed.fits' })];
    renderTab([libraryFrame({ frameUuid: 'u-1', night: '2026-09-28' })]);

    const row = (await screen.findByText('landed.fits')).closest('tr');
    expect(row).not.toBeNull();
    expect(within(row as HTMLElement).getByText('2026-09-28')).toBeInTheDocument();
  });

  it('renders a dash for night when the frame has not landed yet (no mirror entry)', async () => {
    moderationItems = [mod({ frameUuid: 'u-2', fileName: 'pending.fits' })];
    renderTab([]);

    const row = (await screen.findByText('pending.fits')).closest('tr');
    // Column order for the moderation table's default columns is
    // name, publisher, night, filter, … — index 3 (0 = the row checkbox).
    const cells = within(row as HTMLElement).getAllByRole('cell');
    expect(cells[3]).toHaveTextContent('—');
  });
});

describe('ModerationTab — Waiting for review, approval off', () => {
  it('shows the muted line and never calls list_collab_moderation', async () => {
    renderTab(null, { requireApproval: false });

    expect(await screen.findByText('This project publishes without review.')).toBeInTheDocument();
    expect(screen.queryByText('Nothing waiting for review.')).not.toBeInTheDocument();
    expect(vi.mocked(api.invoke).mock.calls.some(([cmd]) => cmd === 'list_collab_moderation')).toBe(false);
  });
});

describe('ModerationTab — Excluded frames', () => {
  function excludedFrame(overrides: Partial<ProjectFrameView> = {}): ProjectFrameView {
    return libraryFrame({ accepted: false, acceptedReason: 'wrong target', ...overrides });
  }

  it('shows the empty text when nothing is excluded', async () => {
    renderTab([libraryFrame({ frameUuid: 'u-1', accepted: true })], { requireApproval: false });
    expect(await screen.findByText('No frames are excluded.')).toBeInTheDocument();
  });

  it('lists every excluded frame with its reason; an accepted frame does not appear', async () => {
    const normal = libraryFrame({ frameUuid: 'n-1', fileName: 'ok.fits', accepted: true });
    const excludedA = excludedFrame({ frameUuid: 'e-1', fileName: 'bad1.fits', acceptedReason: 'trailed' });
    const excludedB = excludedFrame({ frameUuid: 'e-2', fileName: 'bad2.fits', acceptedReason: 'wrong target' });
    renderTab([normal, excludedA, excludedB], { requireApproval: false });

    await screen.findByText('bad1.fits');
    expect(screen.getByText('bad2.fits')).toBeInTheDocument();
    expect(screen.queryByText('ok.fits')).not.toBeInTheDocument();
    expect(screen.getByText('trailed')).toBeInTheDocument();
    expect(screen.getByText('wrong target')).toBeInTheDocument();
  });

  it('Restore 2 invokes restore_collab_frame for each selected frame, then onDecided', async () => {
    const excludedA = excludedFrame({ frameUuid: 'e-1', fileName: 'bad1.fits' });
    const excludedB = excludedFrame({ frameUuid: 'e-2', fileName: 'bad2.fits' });
    const { onDecided } = renderTab([excludedA, excludedB], { requireApproval: false });

    await screen.findByText('bad1.fits');
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Restore 2' }));

    await waitFor(() => expect(restoreCalls()).toHaveLength(2));
    expect(restoreCalls()).toEqual([
      { projectId: 'proj-1', frameUuid: 'e-1' },
      { projectId: 'proj-1', frameUuid: 'e-2' },
    ]);
    await waitFor(() => expect(onDecided).toHaveBeenCalledTimes(1));
  });

  it('a failure on the second of two stops the batch and reads "Restored 1 of 2 — …"', async () => {
    const excludedA = excludedFrame({ frameUuid: 'e-1', fileName: 'bad1.fits' });
    const excludedB = excludedFrame({ frameUuid: 'e-2', fileName: 'bad2.fits' });
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'list_collab_moderation') return Promise.resolve(moderationItems);
      if (command === 'restore_collab_frame') {
        const n = restoreCalls().length;
        if (n === 2) return Promise.reject(new Error('offline'));
        return Promise.resolve(null);
      }
      return Promise.resolve(null);
    }) as never);

    const { onDecided } = renderTab([excludedA, excludedB], { requireApproval: false });
    await screen.findByText('bad1.fits');
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Restore 2' }));

    expect(await screen.findByText(/Restored 1 of 2 — offline/)).toBeInTheDocument();
    expect(restoreCalls()).toHaveLength(2);
    expect(restoreCalls().map((c) => c.frameUuid)).toEqual(['e-1', 'e-2']);
    await waitFor(() => expect(onDecided).toHaveBeenCalledTimes(1));
  });
});
