import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../../api';
import { routeInvoke } from '../../test/apiMock';
import { McpPlanProvider } from '../../contexts/McpPlanContext';
import { PlanWindow } from './PlanWindow';
import { plan } from './planFixtures';
import type { McpPlanView } from '../../types/mcp';

vi.mock('../../api');

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

type Handler = (p: { planId: string }) => void;
let handlers: Set<Handler>;
let plans: Record<string, McpPlanView>;
let newest: McpPlanView | null;

const emit = async (planId: string) => {
  await act(async () => {
    [...handlers].forEach((h) => h({ planId }));
  });
};

function mount() {
  return render(
    <StrictMode>
      <McpPlanProvider>
        <PlanWindow />
      </McpPlanProvider>
    </StrictMode>,
  );
}

// The plan is on screen, waiting; then the store moves it to `next` (an event for the same plan).
async function showThen(next: McpPlanView) {
  newest = plans['plan-1'] = plan();
  mount();
  await screen.findByRole('dialog');
  plans['plan-1'] = next;
  await emit('plan-1');
}

// Apply arms one second after the window shows a waiting plan.
const armed = () =>
  waitFor(() => expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(false), { timeout: 2500 });

const answers = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'answer_mcp_plan');

beforeEach(() => {
  handlers = new Set();
  plans = {};
  newest = null;
  notifyMock.mockReset();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation(((_name: string, cb: Handler) => {
    handlers.add(cb);
    return Promise.resolve(() => handlers.delete(cb));
  }) as never);
  routeInvoke({
    get_mcp_plan: (a: { planId: string | null }) => (a.planId ? (plans[a.planId] ?? null) : newest),
    answer_mcp_plan: (a: { planId: string; answer: { action: string } }) => ({
      ...plans[a.planId],
      state: { kind: a.answer.action === 'cancel' ? 'cancelled' : 'applying' },
    }),
  });
});
afterEach(() => cleanup());

describe('PlanWindow', () => {
  it('opens_on_event_and_on_mount', async () => {
    // A page opened later finds the waiting plan; the key is always sent.
    newest = plan();
    plans['plan-1'] = newest;
    const first = mount();
    expect(await screen.findByRole('dialog', { name: /Move duplicate copies/ })).toBeTruthy();
    expect(api.invoke).toHaveBeenCalledWith('get_mcp_plan', { planId: null });
    expect(screen.getByText(/athenaeum-dev 0\.7\.1 · catalog \/home\/me\/catalog\.db/)).toBeTruthy();
    // StrictMode mounted twice: exactly one listener is alive.
    expect(handlers.size).toBe(1);
    first.unmount();
    expect(handlers.size).toBe(0);

    // Nothing waits at first; a plan arrives by event.
    newest = null;
    plans = {};
    mount();
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_mcp_plan', { planId: null }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(handlers.size).toBe(1);
    plans['plan-2'] = plan({ planId: 'plan-2' });
    await emit('plan-2');
    expect(await screen.findByRole('dialog')).toBeTruthy();
    expect(api.invoke).toHaveBeenCalledWith('get_mcp_plan', { planId: 'plan-2' });
  });

  it('cancel_sends_cancel', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(answers()).toHaveLength(1));
    expect(answers()[0][1]).toEqual({ planId: 'plan-1', answer: { action: 'cancel' } });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('apply_sends_the_selection', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    expect(screen.getByText('3 files · 6.8 KB')).toBeTruthy();
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include all in M42 Flat (2 copies)' }));
    expect(screen.getByText('2 files · 2.9 KB')).toBeTruthy();
    await armed();
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await waitFor(() => expect(answers()).toHaveLength(1));
    expect(answers()[0][1]).toEqual({
      planId: 'plan-1',
      answer: {
        action: 'apply',
        selection: {
          groups: [
            { groupId: 'group:1', chosen: 'item:1', included: ['item:2', 'item:3'] },
            { groupId: 'group:2', chosen: 'item:4', included: [] },
          ],
        },
      },
    });
    // The window now shows the plan being applied; nothing can be pressed twice.
    expect(await screen.findByText(/Applying/)).toBeTruthy();
  });

  it('picking_another_copy_that_stays_is_sent_and_blocks_apply_until_one_is_chosen', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    fireEvent.click(screen.getByRole('radio', { name: 'Copy that stays: copy-2.fits' }));
    expect((screen.getByRole('checkbox', { name: 'Include copy-2.fits' }) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole('checkbox', { name: 'Include copy-1.fits' }) as HTMLInputElement).checked).toBe(false);
    await armed();
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await waitFor(() => expect(answers()).toHaveLength(1));
    const sel = (answers()[0][1] as { answer: { selection: { groups: { chosen: string; included: string[] }[] } } }).answer.selection;
    expect(sel.groups[0]).toMatchObject({ chosen: 'item:2', included: ['item:3'] });
  });

  it('a_locked_copy_can_be_the_one_that_stays', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    const radio = (n: number) => screen.getByRole('radio', { name: `Copy that stays: copy-${n}.fits` }) as HTMLInputElement;
    expect(radio(6).disabled).toBe(false);
    fireEvent.click(radio(6));
    expect(radio(6).checked).toBe(true);
    // The locked copy that stays still cannot be ticked.
    expect((screen.getByRole('checkbox', { name: 'Include copy-6.fits' }) as HTMLInputElement).disabled).toBe(true);
    // A misclick back to the planned keeper works.
    fireEvent.click(radio(4));
    expect(radio(4).checked).toBe(true);
    expect(radio(6).checked).toBe(false);
  });

  it('a_partly_ticked_group_shows_half_ticked', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    const group = () => screen.getByRole('checkbox', { name: 'Include all in M31 Light 300 s (3 copies)' }) as HTMLInputElement;
    expect(group().checked).toBe(true);
    expect(group().indeterminate).toBe(false);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include copy-3.fits' }));
    expect(group().checked).toBe(false);
    expect(group().indeterminate).toBe(true);
    // One click on a half-ticked group unticks all of its copies.
    fireEvent.click(group());
    expect(group().indeterminate).toBe(false);
    expect(group().checked).toBe(false);
    expect((screen.getByRole('checkbox', { name: 'Include copy-2.fits' }) as HTMLInputElement).checked).toBe(false);
  });

  it('locked_item_is_greyed_with_its_reason_and_cannot_be_ticked', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    expect(screen.getByText('calibration links point at it; it stays in v1')).toBeTruthy();
    expect((screen.getByRole('checkbox', { name: 'Include copy-6.fits' }) as HTMLInputElement).disabled).toBe(true);
  });

  it('a_preparing_plan_shows_progress_and_turns_waiting_by_event', async () => {
    const preparing = plan({ state: { kind: 'preparing', done: 3, total: 12 }, groups: [], expiresAt: null });
    newest = plans['plan-1'] = preparing;
    mount();
    expect(await screen.findByText(/3 of 12/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(true);
    plans['plan-1'] = plan();
    await emit('plan-1');
    await waitFor(() => expect(screen.getByText('3 files · 6.8 KB')).toBeTruthy());
    await armed();
  });

  it('shows_the_report_with_reasons', async () => {
    const line = (n: number, reason: string) => ({ itemId: `item:${n}`, fileName: `copy-${n}.fits`, folder: `/data/night-${n}`, reason });
    await showThen(plan({
      state: { kind: 'applied' },
      report: {
        applied: [line(2, 'moved to the Black Hole')],
        skipped: [line(3, 'changed on disk since the plan (size 2000 → 2100)')],
        failed: [line(5, 'permission denied')],
        runId: null,
      },
    }));
    expect(await screen.findByText('changed on disk since the plan (size 2000 → 2100)')).toBeTruthy();
    expect(screen.getByText('permission denied')).toBeTruthy();
    expect(screen.getByText('moved to the Black Hole')).toBeTruthy();
    expect(screen.getByText(/1 applied/)).toBeTruthy();
    expect(screen.getByText(/1 skipped/)).toBeTruthy();
    expect(screen.getByText(/1 failed/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
    // The report stays until the user dismisses it.
    const closes = screen.getAllByRole('button', { name: 'Close' });
    fireEvent.click(closes[closes.length - 1]);
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('expired_plan_offers_no_apply', async () => {
    await showThen(plan({ state: { kind: 'expired' } }));
    expect(await screen.findByText(/expired/i)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
  });

  it('a_finished_plan_does_not_open_a_closed_window', async () => {
    newest = plans['plan-1'] = plan({ state: { kind: 'applied' } });
    mount();
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_mcp_plan', { planId: null }));
    plans['plan-0'] = plan({ planId: 'plan-0', state: { kind: 'expired' } });
    await emit('plan-0');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('a_failed_plan_says_why', async () => {
    await showThen(plan({ state: { kind: 'failed', error: 'the catalog went away' }, groups: [] }));
    expect(await screen.findByText(/the catalog went away/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
  });

  it('replaced_plan_stays_until_the_user_opens_the_new_one', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    // A newer plan is prepared while this one is on screen: the shown plan does not change.
    plans['plan-2'] = plan({ planId: 'plan-2', title: 'The newer plan' });
    await emit('plan-2');
    expect(screen.getByRole('dialog', { name: /Move duplicate copies/ })).toBeTruthy();
    expect(screen.queryByText('The newer plan')).toBeNull();
    // The store replaces the old plan: still the same plan, now with the banner, Apply disabled.
    plans['plan-1'] = plan({ state: { kind: 'replaced' }, replacedBy: 'plan-2' });
    await emit('plan-1');
    expect(await screen.findByText('The assistant prepared a newer plan')).toBeTruthy();
    expect(screen.getByRole('dialog', { name: /Move duplicate copies/ })).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Open the new plan' }));
    expect(await screen.findByRole('dialog', { name: /The newer plan/ })).toBeTruthy();
    expect(screen.queryByText('The assistant prepared a newer plan')).toBeNull();
    await armed();
  });

  it('apply_has_no_initial_focus', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    const dialog = await screen.findByRole('dialog');
    // No button has the focus: a Space or Enter meant for the page under it presses nothing.
    const neither = (root: HTMLElement) => {
      const active = document.activeElement as HTMLElement;
      expect(active.tagName).not.toBe('BUTTON');
      expect(root.contains(active)).toBe(true);
    };
    neither(dialog);
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: 'Enter' });
    fireEvent.keyUp(document.activeElement as HTMLElement, { key: 'Enter' });
    expect(answers()).toHaveLength(0);
    // A plan switch lands on the title again, not on a button.
    plans['plan-1'] = plan({ state: { kind: 'replaced' }, replacedBy: 'plan-2' });
    plans['plan-2'] = plan({ planId: 'plan-2', title: 'The newer plan' });
    await emit('plan-1');
    fireEvent.click(await screen.findByRole('button', { name: 'Open the new plan' }));
    neither(await screen.findByRole('dialog', { name: /The newer plan/ }));
    expect(answers()).toHaveLength(0);
  });

  it('a_click_outside_does_not_cancel', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    fireEvent.mouseDown(screen.getByTestId('dialog-scrim'));
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    expect(answers()).toHaveLength(0);
    expect(screen.getByRole('dialog')).toBeTruthy();
    // Escape is a deliberate act: it cancels.
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(answers()).toHaveLength(1));
    expect(answers()[0][1]).toEqual({ planId: 'plan-1', answer: { action: 'cancel' } });
  });

  it('a_click_outside_does_not_cancel_an_applied_plan_either', async () => {
    const line = { itemId: 'item:2', fileName: 'copy-2.fits', folder: '/data/night-2', reason: 'moved to the Black Hole' };
    await showThen(plan({ state: { kind: 'applied' }, report: { applied: [line], skipped: [], failed: [], runId: null } }));
    await screen.findByText('moved to the Black Hole');
    fireEvent.mouseDown(screen.getByTestId('dialog-scrim'));
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    // The report is the only place the user sees what moved: a stray click leaves it open.
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('moved to the Black Hole')).toBeTruthy();
  });

  it('an_applying_plan_can_be_closed', async () => {
    const closers: Record<string, () => void> = {
      escape: () => fireEvent.keyDown(document, { key: 'Escape' }),
      cross: () => fireEvent.click(screen.getAllByRole('button', { name: 'Close' })[0]),
      footer: () => {
        const all = screen.getAllByRole('button', { name: 'Close' });
        fireEvent.click(all[all.length - 1]);
      },
    };
    for (const [how, close] of Object.entries(closers)) {
      await showThen(plan({ state: { kind: 'applying' } }));
      expect(screen.getByText('Applying… you can close this window; the report opens when it is done.'), how).toBeTruthy();
      const buttons = screen.getAllByRole('button', { name: 'Close' });
      expect(buttons.every((b) => !(b as HTMLButtonElement).disabled), how).toBe(true);
      close();
      await waitFor(() => expect(screen.queryByRole('dialog'), how).toBeNull());
      // Closing hides the window; it never cancels the apply.
      expect(answers(), how).toHaveLength(0);
      cleanup();
    }
  });

  it('the_report_opens_when_a_hidden_apply_ends', async () => {
    await showThen(plan({ state: { kind: 'applying' } }));
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    // Progress events for the hidden plan do not bring the window back.
    await emit('plan-1');
    expect(screen.queryByRole('dialog')).toBeNull();
    // A newer plan waits unseen behind it.
    plans['plan-2'] = plan({ planId: 'plan-2', title: 'The newer plan' });
    await emit('plan-2');
    expect(screen.queryByRole('dialog')).toBeNull();
    // The end state opens the window again, with the report, and without a notification.
    const line = { itemId: 'item:2', fileName: 'copy-2.fits', folder: '/data/night-2', reason: 'moved to the Black Hole' };
    plans['plan-1'] = plan({ state: { kind: 'applied' }, report: { applied: [line], skipped: [], failed: [], runId: null } });
    await emit('plan-1');
    expect(await screen.findByRole('dialog', { name: /Move duplicate copies/ })).toBeTruthy();
    expect(screen.getByText('moved to the Black Hole')).toBeTruthy();
    expect(notifyMock).not.toHaveBeenCalled();
    // Close on the report then opens the newer plan.
    const closes = screen.getAllByRole('button', { name: 'Close' });
    fireEvent.click(closes[closes.length - 1]);
    expect(await screen.findByRole('dialog', { name: /The newer plan/ })).toBeTruthy();
  });

  it('a_lost_event_is_caught_up_by_the_re_read', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    try {
      newest = plans['plan-1'] = plan({ state: { kind: 'applying' } });
      mount();
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      await tick(0);
      expect(screen.getByRole('dialog')).toBeTruthy();
      // The plan ends, but the event is lost: nothing tells the window.
      const line = { itemId: 'item:2', fileName: 'copy-2.fits', folder: '/data/night-2', reason: 'moved to the Black Hole' };
      plans['plan-1'] = plan({ state: { kind: 'applied' }, report: { applied: [line], skipped: [], failed: [], runId: null } });
      expect(screen.queryByText('moved to the Black Hole')).toBeNull();
      await tick(3100);
      expect(screen.getByText('moved to the Black Hole')).toBeTruthy();
      expect(api.invoke).toHaveBeenCalledWith('get_mcp_plan', { planId: 'plan-1' });
      // The plan has ended: the timer is gone.
      const reads = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_mcp_plan').length;
      const before = reads();
      await tick(9000);
      expect(reads()).toBe(before);
    } finally {
      vi.useRealTimers();
    }
  });

  it('a_visible_tab_re_reads_the_plan_and_unmount_stops_the_timer', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    try {
      newest = plans['plan-1'] = plan({ state: { kind: 'applying' } });
      const view = mount();
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      await tick(0);
      const reads = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_mcp_plan').length;
      const before = reads();
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')); });
      expect(reads()).toBe(before + 1);
      view.unmount();
      const after = reads();
      await tick(9000);
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')); });
      expect(reads()).toBe(after);
    } finally {
      vi.useRealTimers();
    }
  });

  it('outside_waiting_the_window_shows_the_plans_own_selection', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    // This window's own ticks: group 1 is unticked here, so only the group-2 copy counts.
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include all in M31 Light 300 s (3 copies)' }));
    expect(screen.getByText('1 file · 3.9 KB')).toBeTruthy();
    // Another page applied only copy-3 of the first group: the backend recorded exactly that.
    const applied = plan({ state: { kind: 'applying' } });
    applied.groups[0].items[1].selected = false;
    applied.groups[1].items[1].selected = false;
    plans['plan-1'] = applied;
    await emit('plan-1');
    await waitFor(() => expect(screen.getByText('1 file · 2.0 KB')).toBeTruthy());
    expect((screen.getByRole('checkbox', { name: 'Include copy-3.fits' }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole('checkbox', { name: 'Include copy-2.fits' }) as HTMLInputElement).checked).toBe(false);
  });

  it('a_plan_that_failed_during_apply_shows_what_was_done', async () => {
    const line = (n: number, reason: string) => ({ itemId: `item:${n}`, fileName: `copy-${n}.fits`, folder: `/data/night-${n}`, reason });
    await showThen(plan({
      state: { kind: 'failed', error: 'the Black Hole folder is full' },
      report: { applied: [line(2, 'moved to the Black Hole')], skipped: [], failed: [line(3, 'no space left')], runId: null },
    }));
    expect(await screen.findByText(/This plan failed: the Black Hole folder is full/)).toBeTruthy();
    expect(screen.getByText('moved to the Black Hole')).toBeTruthy();
    expect(screen.getByText('no space left')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
  });

  it('an_applying_plan_shows_no_newer_plan_banner', async () => {
    await showThen(plan({ state: { kind: 'applying' } }));
    plans['plan-2'] = plan({ planId: 'plan-2', title: 'The newer plan' });
    await emit('plan-2');
    expect(screen.queryByText('The assistant prepared a newer plan')).toBeNull();
    expect(screen.getByRole('dialog', { name: /Move duplicate copies/ })).toBeTruthy();
    // When this plan ends, Close opens the newer one.
    plans['plan-1'] = plan({ state: { kind: 'applied' }, report: { applied: [], skipped: [], failed: [], runId: null } });
    await emit('plan-1');
    const closes = screen.getAllByRole('button', { name: 'Close' });
    fireEvent.click(closes[closes.length - 1]);
    expect(await screen.findByRole('dialog', { name: /The newer plan/ })).toBeTruthy();
  });

  it('apply_arms_after_a_second', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      newest = plans['plan-1'] = plan();
      mount();
      const flush = () => act(async () => { await vi.advanceTimersByTimeAsync(0); });
      await flush();
      const apply = () => screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement;
      expect(apply().disabled).toBe(true);
      await act(async () => { await vi.advanceTimersByTimeAsync(900); });
      expect(apply().disabled).toBe(true);
      await act(async () => { await vi.advanceTimersByTimeAsync(150); });
      expect(apply().disabled).toBe(false);
      // A preparing plan that turns waiting arms again after its own second.
      plans['plan-1'] = plan({ state: { kind: 'preparing', done: 1, total: 2 }, groups: [] });
      await emit('plan-1');
      plans['plan-1'] = plan();
      await emit('plan-1');
      await flush();
      expect(apply().disabled).toBe(true);
      await act(async () => { await vi.advanceTimersByTimeAsync(1100); });
      expect(apply().disabled).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('a_failed_command_is_logged_and_notified_and_the_window_stays', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    newest = plans['plan-1'] = plan();
    routeInvoke({
      get_mcp_plan: () => newest,
      answer_mcp_plan: () => Promise.reject(new Error('plan is not waiting')),
    });
    mount();
    await screen.findByRole('dialog');
    await armed();
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await waitFor(() => expect(notifyMock).toHaveBeenCalled());
    expect(err).toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeTruthy();
    err.mockRestore();
  });

  it('each_plan_kind_has_its_own_words', async () => {
    const line = { itemId: 'item:2', fileName: 'copy-2.fits', folder: '/data/night-2', reason: 'started in run solve-abc' };
    // Applying: one sentence per kind.
    const applying: Array<[McpPlanView['kind'], RegExp]> = [
      ['black_hole', /Files are being moved/],
      ['metadata', /Writing the values to the catalog…/],
      ['plate_solve', /Starting the plate solve…/],
    ];
    for (const [kind, words] of applying) {
      await showThen(plan({ kind, state: { kind: 'applying' } }));
      expect(await screen.findByText(words), kind).toBeTruthy();
      if (kind !== 'black_hole') expect(screen.queryByText(/Files are being moved/), kind).toBeNull();
      cleanup();
    }
    // Applied: a plate-solve plan started a run (it solved nothing yet) and has no "applied" chip.
    await showThen(
      plan({
        kind: 'plate_solve',
        state: { kind: 'applied' },
        report: { applied: [line, { ...line, itemId: 'item:3' }], skipped: [], failed: [], runId: 'solve-abc' },
      }),
    );
    expect(
      await screen.findByText('Started a plate solve of 2 frames (run solve-abc). Its progress shows in the plate-solve panel.'),
    ).toBeTruthy();
    expect(screen.queryByText(/\d+ applied/)).toBeNull();
    cleanup();
    // Failed: the error, in "Nothing was started" words.
    await showThen(
      plan({
        kind: 'plate_solve',
        state: { kind: 'failed', error: 'A plate solve batch started in Athenaeum is running (1 of 4 frames).' },
        report: { applied: [], skipped: [], failed: [{ ...line, reason: 'busy' }], runId: null },
      }),
    );
    expect(await screen.findByText(/Nothing was started: A plate solve batch started in Athenaeum is running/)).toBeTruthy();
    cleanup();
    // A plate-solve plan that failed before any report: the same words, never "Preparing the plan failed".
    await showThen(
      plan({ kind: 'plate_solve', state: { kind: 'failed', error: 'no catalog' }, report: null, groups: [] }),
    );
    expect(await screen.findByText(/Nothing was started: no catalog/)).toBeTruthy();
    expect(screen.queryByText(/Preparing the plan failed/)).toBeNull();
    cleanup();
    // The footer of a plate-solve plan without sizes counts frames and shows no byte total.
    const unsized = plan({ kind: 'plate_solve' });
    await showThen({
      ...unsized,
      groups: unsized.groups.map((g) => ({ ...g, chooseOne: null, items: g.items.map((i) => ({ ...i, sizeBytes: null })) })),
    });
    expect(await screen.findByText(/^\d+ frames?$/)).toBeTruthy();
    expect(screen.queryByText(/· 0 B/)).toBeNull();
    cleanup();
    // A metadata plan counts frames too, with its sizes.
    await showThen(plan({ kind: 'metadata' }));
    expect(await screen.findByText(/^\d+ frames? · /)).toBeTruthy();
    expect(screen.queryByText(/files?/)).toBeNull();
    cleanup();
    // A Black Hole plan keeps its "applied" chip.
    await showThen(
      plan({ state: { kind: 'applied' }, report: { applied: [{ ...line, reason: 'moved to the Black Hole' }], skipped: [], failed: [], runId: null } }),
    );
    expect(await screen.findByText(/1 applied/)).toBeTruthy();
  });

  it('an_empty_failed_report_says_nothing_happened', async () => {
    const empty = { applied: [], skipped: [], failed: [], runId: null };
    const cases: Array<[McpPlanView['kind'], string]> = [
      ['black_hole', 'No file was recorded as moved.'],
      ['metadata', 'Nothing was written.'],
    ];
    for (const [kind, words] of cases) {
      await showThen(plan({ kind, state: { kind: 'failed', error: 'the catalog went away' }, report: empty }));
      expect(await screen.findByText(words), kind).toBeTruthy();
      cleanup();
    }
    // A plate-solve plan uses its own words; a plan that failed while preparing has no report and says no more.
    await showThen(plan({ kind: 'plate_solve', state: { kind: 'failed', error: 'no catalog' }, report: empty }));
    expect(await screen.findByText(/Nothing was started: no catalog/)).toBeTruthy();
    expect(screen.queryByText('No file was recorded as moved.')).toBeNull();
    cleanup();
    await showThen(plan({ state: { kind: 'failed', error: 'the catalog went away' }, groups: [] }));
    expect(await screen.findByText(/Preparing the plan failed/)).toBeTruthy();
    expect(screen.queryByText('No file was recorded as moved.')).toBeNull();
  });

  it('a_plan_that_vanished_stops_the_poll', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      const reads = (id: string) =>
        vi.mocked(api.invoke).mock.calls.filter(([c, a]) => c === 'get_mcp_plan' && (a as { planId: string | null }).planId === id).length;

      // The server restarted while plan-1 applied and the window was hidden; a newer plan waits.
      newest = plans['plan-1'] = plan({ state: { kind: 'applying' } });
      mount();
      await tick(0);
      fireEvent.keyDown(document, { key: 'Escape' });
      await tick(0);
      expect(screen.queryByRole('dialog')).toBeNull();
      plans['plan-2'] = plan({ planId: 'plan-2', title: 'The newer plan' });
      await emit('plan-2');
      expect(screen.queryByRole('dialog')).toBeNull();
      delete plans['plan-1'];
      await tick(3100);
      // The window is back, on the newer plan, and plan-1 is not read again.
      expect(screen.getByRole('dialog', { name: /The newer plan/ })).toBeTruthy();
      expect(warn).toHaveBeenCalledWith(expect.stringContaining('gone'), 'plan-1');
      const before = reads('plan-1');
      await tick(9000);
      expect(reads('plan-1')).toBe(before);
      cleanup();

      // No newer plan: the plan is cleared and nothing is polled.
      vi.mocked(api.invoke).mockClear();
      plans = {};
      newest = plans['plan-3'] = plan({ planId: 'plan-3', state: { kind: 'applying' } });
      mount();
      await tick(0);
      expect(screen.getByRole('dialog')).toBeTruthy();
      delete plans['plan-3'];
      newest = null;
      await tick(3100);
      expect(screen.queryByRole('dialog')).toBeNull();
      const after = reads('plan-3');
      await tick(9000);
      expect(reads('plan-3')).toBe(after);
    } finally {
      warn.mockRestore();
      vi.useRealTimers();
    }
  });
});
