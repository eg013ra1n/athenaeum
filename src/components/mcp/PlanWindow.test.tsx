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
});
