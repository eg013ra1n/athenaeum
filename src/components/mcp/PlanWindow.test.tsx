import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../../api';
import { routeInvoke } from '../../test/apiMock';
import { McpPlanProvider } from '../../contexts/McpPlanContext';
import { PlanWindow } from './PlanWindow';
import { formatBytes } from '../duplicates/presentation';
import { blackHoleMockupPlan, count, metadataPlan, metadataVolumePlan, plan, plateSolvePlan, plateSolveVolumePlan } from './planFixtures';
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

// The header's live headline and the footer's "N of M", as the user reads them.
const stats = () => screen.getByTestId('plan-stats').textContent ?? '';
const picked = () => screen.getByTestId('plan-selection').textContent ?? '';

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
    // The build and catalog are reachable on the info icon, not printed in the header.
    expect(screen.getByTitle('athenaeum-dev 0.7.1 · catalog /home/me/catalog.db')).toBeTruthy();
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
    expect(stats()).toContain(`3 files to the Black Hole·${formatBytes(7000)} will free`);
    expect(picked()).toBe(`3 of 6 files marked · ${formatBytes(7000)} will free`);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include all in M42 Flat (2 copies)' }));
    expect(stats()).toContain(`2 files to the Black Hole·${formatBytes(3000)} will free`);
    expect(picked()).toBe(`2 of 6 files marked · ${formatBytes(3000)} will free`);
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
    fireEvent.click(screen.getByRole('button', { name: 'Copy that stays: copy-2.fits' }));
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
    // The copy that stays offers no "keep" button of its own; every other candidate does.
    const keep = (n: number) => screen.queryByRole('button', { name: `Copy that stays: copy-${n}.fits` }) as HTMLButtonElement | null;
    expect(keep(6)!.disabled).toBe(false);
    fireEvent.click(keep(6)!);
    expect(keep(6)).toBeNull();
    expect(keep(4)).toBeTruthy();
    // The locked copy that stays still cannot be ticked.
    expect((screen.getByRole('checkbox', { name: 'Include copy-6.fits' }) as HTMLInputElement).disabled).toBe(true);
    // A misclick back to the planned keeper works.
    fireEvent.click(keep(4)!);
    expect(keep(4)).toBeNull();
    expect(keep(6)).toBeTruthy();
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
    await waitFor(() => expect(stats()).toContain(`3 files to the Black Hole·${formatBytes(7000)} will free`));
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
    expect(stats()).toContain(`1 file to the Black Hole·${formatBytes(4000)} will free`);
    // Another page applied only copy-3 of the first group: the backend recorded exactly that.
    const applied = plan({ state: { kind: 'applying' } });
    applied.groups[0].items[1].selected = false;
    applied.groups[1].items[1].selected = false;
    plans['plan-1'] = applied;
    await emit('plan-1');
    await waitFor(() => expect(stats()).toContain(`1 file to the Black Hole·${formatBytes(2000)} will free`));
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
    await screen.findByTestId('plan-selection');
    expect(picked()).toMatch(/^\d+ of \d+ frames selected$/);
    expect(stats()).not.toMatch(/0 B|will free/);
    cleanup();
    // A metadata plan counts values, and a size is the Black Hole's alone.
    await showThen(plan({ kind: 'metadata' }));
    await screen.findByTestId('plan-selection');
    expect(picked()).toMatch(/^\d+ of \d+ values selected$/);
    expect(stats()).toMatch(/\d+ values to write/);
    expect(stats()).not.toMatch(/files?|will free/);
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

describe('PlanWindow in the Duplicates page\'s language', () => {
  const row = (fileName: string) => screen.getByText(fileName).closest('[data-testid="plan-row"]') as HTMLElement;

  it('the_copy_that_stays_never_reads_move', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    // copy-1 stays: it says so and never the group's action; the ticked copies say where they go.
    expect(row('copy-1.fits').textContent).toMatch(/Stays/);
    expect(row('copy-1.fits').textContent).not.toMatch(/Black Hole/);
    expect(row('copy-2.fits').textContent).toMatch(/To the Black Hole/);
    // The marker follows the selection: pick copy-2 and the words swap.
    fireEvent.click(screen.getByRole('button', { name: 'Copy that stays: copy-2.fits' }));
    expect(row('copy-2.fits').textContent).toMatch(/Stays/);
    expect(row('copy-2.fits').textContent).not.toMatch(/Black Hole/);
    expect(row('copy-1.fits').textContent).not.toMatch(/Black Hole/);
    // Untick copy-3: it stays too.
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include copy-3.fits' }));
    expect(row('copy-3.fits').textContent).not.toMatch(/Black Hole/);
    // The planner's wording for the whole group is printed nowhere.
    expect(screen.queryByText(/moves to the Black Hole/)).toBeNull();
  });

  it('a_locked_row_says_locked_and_why', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    const locked = row('copy-6.fits');
    expect(locked.textContent).toMatch(/Locked/);
    expect(locked.textContent).toMatch(/calibration links point at it; it stays in v1/);
    expect(locked.textContent).not.toMatch(/To the Black Hole/);
  });

  it('labels_are_human_never_header_keywords', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    const dialog = await screen.findByRole('dialog');
    expect(dialog.textContent).not.toMatch(/IMAGETYP|OBJECT|FILTER|EXPTIME|DATE-OBS|INSTRUME/);
    for (const word of ['Object', 'Filter', 'Exposure', 'Camera']) expect(dialog.textContent).toContain(word);
    expect(dialog.textContent).toContain('300 s');
    // What the copies share is said once for the group, not on every row.
    expect(screen.getAllByText('ASI2600MM')).toHaveLength(2);
  });

  it('the_summary_size_is_the_shared_formatters', async () => {
    newest = plans['plan-1'] = plan();
    mount();
    await screen.findByRole('dialog');
    // The headline is the app's byte formatter over the ticks; the expiry is a chip, not part of the headline.
    expect(stats()).toBe(`2 groups·3 files to the Black Hole·${formatBytes(7000)} will free`);
    expect(screen.getByText('Expires 2026-11-20 15:00:00')).toBeTruthy();
    // Unticking lowers it.
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include all in M42 Flat (2 copies)' }));
    expect(stats()).toBe(`2 groups·2 files to the Black Hole·${formatBytes(3000)} will free`);
  });

  it('no_plan_scrolls_sideways', async () => {
    for (const make of [plan, metadataPlan, plateSolvePlan]) {
      newest = plans['plan-1'] = make();
      mount();
      await screen.findByRole('dialog');
      expect(document.body.innerHTML, make.name).not.toContain('overflow-x-auto');
      expect(document.querySelector('table'), make.name).toBeNull();
      cleanup();
    }
  });

  it('a_metadata_plan_shows_what_changes_and_why', async () => {
    newest = plans['plan-1'] = metadataPlan();
    mount();
    const dialog = await screen.findByRole('dialog');
    expect(dialog.textContent).not.toMatch(/OBJECT|INSTRUME/);
    // The field and its why are said once, on the group row; a frame row is name, folder, now -> proposed.
    expect(dialog.textContent).toContain('Object → M31');
    expect(dialog.textContent).toContain('Camera → ASI2600MM');
    // One per group that proposes it (both groups here), never one per frame.
    expect(screen.getAllByText('Why: the folder name says M31')).toHaveLength(2);
    expect(row('frame-1.fits').textContent).toMatch(/\(empty\)M31/);
    expect(row('frame-1.fits').textContent).not.toMatch(/the folder name says M31/);
    expect(row('frame-3.fits').textContent).toMatch(/\(empty\)ASI2600MM/);
    // A frame locked on its own shows the reason as a chip, with the full text on hover.
    const locked = row('frame-2.fits');
    expect(locked.querySelector('[title="the file is on a drive that is not connected"]')).toBeTruthy();
    expect((screen.getByRole('checkbox', { name: 'Include frame-2.fits' }) as HTMLInputElement).disabled).toBe(true);
    expect(stats()).toMatch(/^2 values to write/);
  });

  it('a_plate_solve_plan_shows_the_position_hint_once_per_group_and_per_frame', async () => {
    newest = plans['plan-1'] = plateSolvePlan();
    mount();
    await screen.findByRole('dialog');
    expect(row('light-1.fits').textContent).toContain('RA 10.6847, Dec 41.2690');
    // A frame without a hint has an empty cell; what a solve writes is said once, in the sentence.
    expect(row('light-2.fits').textContent).not.toMatch(/RA 10|Position hint/);
    expect(row('light-1.fits').textContent).not.toMatch(/A solve writes/);
    expect(screen.getAllByText("A solve writes the frame's coordinates.")).toHaveLength(1);
    // The group row carries the evidence, once.
    expect(screen.getAllByText(/No plate solve is stored for this frame/)).toHaveLength(1);
    expect(row('light-3.fits').querySelector('[title="the star catalog is not installed"]')).toBeTruthy();
    expect(stats()).toMatch(/^2 frames to solve/);
  });
});

describe('PlanWindow as the approved mockup', () => {
  const row = (fileName: string) => screen.getByText(fileName).closest('[data-testid="plan-row"]') as HTMLElement;
  const rowsOf = () => screen.queryAllByTestId('plan-row');

  it('the_stats_row_reads_the_summary_with_the_right_number', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan();
    mount();
    await screen.findByRole('dialog');
    expect(stats()).toBe('552 frames to solve·6 folders·Left out: 650 not light frames·339 already solved');
    cleanup();
    // One of anything uses the singular label; the thousands separator is the app's.
    newest = plans['plan-1'] = plateSolveVolumePlan({
      summary: { ...plateSolveVolumePlan().summary, counts: [count(1, 'folder', 'folders')], leftOut: [count(1, 'not a light frame', 'not light frames'), count(1829, 'x', 'frames already solved')] },
    });
    mount();
    await screen.findByRole('dialog');
    expect(stats()).toBe('552 frames to solve·1 folder·Left out: 1 not a light frame·1,829 frames already solved');
  });

  it('the_facts_are_chips_with_their_tone_and_the_sentence_is_said_once', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan({
      summary: {
        ...plateSolveVolumePlan().summary,
        facts: [
          { label: 'Star catalog', value: 'not installed', tone: 'warn' },
          { label: 'Index packs', value: '0', tone: 'neutral' },
          { label: 'At a time', value: '3', tone: 'neutral' },
          { label: 'Star catalog', value: 'installed', tone: 'ok' },
        ],
      },
    });
    mount();
    await screen.findByRole('dialog');
    const warn = screen.getByText('Star catalog not installed');
    expect(warn.className).toMatch(/text-warning/);
    expect(screen.getByText('Index packs 0').className).toMatch(/text-content-muted/);
    expect(screen.getByText('3 at a time').className).toMatch(/text-content-muted/);
    expect(screen.getByText('Star catalog installed').className).toMatch(/text-success/);
    expect(screen.getAllByText(/^A solve writes the frame's coordinates and/)).toHaveLength(1);
  });

  it('no_element_renders_the_notes', async () => {
    for (const make of [plan, metadataPlan, plateSolveVolumePlan]) {
      newest = plans['plan-1'] = make();
      mount();
      const dialog = await screen.findByRole('dialog');
      for (const note of make().notes) expect(dialog.textContent, `${make.name}: ${note}`).not.toContain(note);
      cleanup();
    }
  });

  it('a_plate_solve_frame_is_one_line_and_repeats_nothing_its_group_says', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan();
    mount();
    await screen.findByRole('dialog');
    const dialog = screen.getByRole('dialog');
    // What the six folders share is on the group rows: object, filter, exposure, hint, evidence.
    expect(dialog.textContent).toContain('Object M 78');
    expect(dialog.textContent).toContain('Position hint on 9 frames');
    expect(dialog.textContent).toContain('Position hint none');
    expect(screen.getAllByText(/No plate solve is stored for this frame/)).toHaveLength(6);
    const first = row('2025-09-13_20-31-12__light_0001.fits');
    expect(first.textContent).toBe('2025-09-13_20-31-12__light_0001.fits');
    // The frames of folder 2 that have a hint show it, and only they do.
    expect(row('2025-09-14_20-31-12__light_0003.fits').textContent).toContain('05h 46m 45s');
    expect(row('2025-09-14_20-31-12__light_0004.fits').textContent).toBe('2025-09-14_20-31-12__light_0004.fits');
    expect(screen.getByText('2025-09-14_20-31-12__light_0003.fits').closest('[data-testid="plan-row"]')?.querySelectorAll('input')).toHaveLength(1);
  });

  it('a_group_shows_twelve_frames_then_plus_n_more_which_opens_in_place', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan();
    mount();
    await screen.findByRole('dialog');
    // Six folders, twelve frames each.
    expect(rowsOf()).toHaveLength(6 * 12);
    const more = screen.getByRole('button', { name: '+106 more frames in this folder' });
    fireEvent.click(more);
    expect(rowsOf()).toHaveLength(5 * 12 + 118);
    expect(screen.queryByRole('button', { name: '+106 more frames in this folder' })).toBeNull();
    // The group row says "Solve 118 of 118" and counts with the separator.
    expect(screen.getByRole('dialog').textContent).toContain('Solve 118 of 118');
    expect(screen.getByRole('dialog').textContent).toContain('143 frames');
    // Unticking lowers the headline.
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include all in /Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/2025-09-13/LIGHT' }));
    expect(stats()).toMatch(/^434 frames to solve/);
    expect(picked()).toBe('434 of 552 frames selected');
  });

  it('a_group_that_is_locked_for_one_reason_is_one_row_with_its_count_and_reason', async () => {
    newest = plans['plan-1'] = metadataVolumePlan();
    mount();
    await screen.findByRole('dialog');
    const dialog = screen.getByRole('dialog');
    expect(stats()).toBe('1 value to write·Left out: 1,829 frames with no value found·1 frame leaves its session · 2 calibration links removed');
    expect(dialog.textContent).toContain('1,829 frames');
    // The reason is on the group row once, with its full text on hover; its frames are not rendered.
    expect(screen.getAllByTitle('No value found')).toHaveLength(1);
    expect(rowsOf()).toHaveLength(1);
    expect(screen.getByRole('button', { name: 'Expand Object: nothing proposed' }).getAttribute('aria-expanded')).toBe('false');
    expect((screen.getByRole('checkbox', { name: 'Include all in Object: nothing proposed' }) as HTMLInputElement).disabled).toBe(true);
    // Opened, it shows twelve disabled frames and "+N more".
    fireEvent.click(screen.getByRole('button', { name: 'Expand Object: nothing proposed' }));
    expect(rowsOf()).toHaveLength(1 + 12);
    expect(screen.getByRole('button', { name: '+1,817 more frames in this group' })).toBeTruthy();
    const frame = rowsOf()[1]!;
    expect(frame.textContent).toMatch(/\(empty\)—$/);
    expect(frame.querySelector('input')?.disabled).toBe(true);
    // The proposed group reads "Object → Filter test", with its why once.
    expect(dialog.textContent).toContain('Object → Filter test');
    expect(screen.getAllByText('Why: the other frames of that night carry the same object name')).toHaveLength(1);
  });

  it('collapse_all_and_expand_all_follow_what_is_open', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan();
    mount();
    await screen.findByRole('dialog');
    fireEvent.click(screen.getByRole('button', { name: 'Collapse all' }));
    expect(rowsOf()).toHaveLength(0);
    expect(screen.getByRole('button', { name: 'Expand all' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Expand all' }));
    expect(rowsOf()).toHaveLength(6 * 12);
  });

  it('select_all_in_the_table_head_ticks_and_unticks_every_frame', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan();
    mount();
    await screen.findByRole('dialog');
    const all = () => screen.getByRole('checkbox', { name: 'Select all' }) as HTMLInputElement;
    expect(all().checked).toBe(true);
    fireEvent.click(all());
    expect(picked()).toBe('0 of 552 frames selected');
    fireEvent.click(all());
    expect(picked()).toBe('552 of 552 frames selected');
  });

  it('the_black_hole_cards_are_the_duplicates_ones_with_one_note_per_group', async () => {
    newest = plans['plan-1'] = blackHoleMockupPlan();
    mount();
    await screen.findByRole('dialog');
    const dialog = screen.getByRole('dialog');
    // Counts, headline, size, and the group that needs its copy that stays, live.
    expect(stats()).toContain('3 groups·3 files to the Black Hole·');
    expect(stats()).toContain('1 group needs a copy that stays');
    expect(picked()).toMatch(/^3 of 7 files marked · .* will free$/);
    // Pills: "Move 2 of 3", and the group with no keeper asks for one.
    expect(dialog.textContent).toContain('Move 2 of 3');
    expect(dialog.textContent).toContain('Move 1 of 2');
    expect(dialog.textContent).toContain('Pick the copy that stays');
    // The master's lock is a chip, "Stays · 3 links", with the full reason on hover.
    expect(screen.getByTitle('3 calibration links point at this copy, so it stays').textContent).toContain('Stays · 3 links');
    // The unreachable keeper is an amber line on the group.
    expect(screen.getByText(/cannot be reached/).className).toMatch(/text-warning/);
    // The byte check is said once per group, not per row.
    expect(screen.getAllByText('identical bytes to the copy that stays')).toHaveLength(3);
    // Picking a copy that stays in group 3 clears the warning.
    fireEvent.click(screen.getAllByRole('button', { name: /^Copy that stays: 2025-09-12_22-41-07/ })[0]!);
    expect(stats()).not.toContain('needs a copy that stays');
    expect(dialog.textContent).not.toContain('Pick the copy that stays');
  });

  it('the_list_scrolls_while_the_header_the_sentence_and_the_footer_stay', async () => {
    newest = plans['plan-1'] = plateSolveVolumePlan();
    mount();
    const dialog = await screen.findByRole('dialog');
    const list = screen.getByTestId('plan-list');
    // The scrolling container is the list; the dialog body around it is a flex column that does not scroll.
    expect(list.className).toMatch(/overflow-y-auto/);
    const body = list.parentElement!;
    expect(body.className).not.toMatch(/overflow/);
    expect(body.className).toMatch(/flex-col/);
    // Stats, chips, sentence and the footer sit outside the list.
    for (const fixed of [screen.getByTestId('plan-stats'), screen.getByTestId('plan-header'), screen.getByTestId('plan-selection'), screen.getByRole('button', { name: 'Cancel' })]) {
      expect(list.contains(fixed)).toBe(false);
    }
    expect(screen.getByText(/^A solve writes the frame's coordinates and/).closest('[data-testid="plan-list"]')).toBeNull();
    // The table header is the first thing in the list and sticks to its top.
    expect(screen.getByTestId('plan-table-head').className).toMatch(/sticky top-0/);
    expect(dialog.contains(list)).toBe(true);
  });
});
