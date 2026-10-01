# Collab publish review — wave 2 (frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put wave 1's core on the project page:
- the Overview with My contribution first and a Project settings card (publishing device, publishing mode, auto-replicate);
- My frames with a To review segment, Calibrate / Publish / Don't publish / Release / Update, and a run panel with Cancel;
- a Live pill that waits for the hub's confirmation and a page that refreshes on changes;
- Blink in a project, showing raw / calibrated / replica frames with role-gated actions and no Black Hole.

**Architecture:**
- **Data:** the page keeps its loaders. Two new hooks carry the new data:
  - `useCollabPublishRun` reads the run snapshot and follows the run events.
  - The pill follows `collab-project-synced`.
- **Overview:** reorders its cards and adds `ProjectSettingsCard`. `MetaLine` is deleted.
- **My frames:** gains a `review` segment, new `TableAction`s and a `PublishRunPanel`.
- **Blink:** `BlinkViewer` gets an opt-in project mode (`actions`, `imageRef`, `contextLabel`, `viewOnly`), and every existing caller is unchanged. A `ProjectBlink` component wires it to the tables.

**Tech Stack:** React 18 + TypeScript, Tailwind with design tokens, `src/components/ui/` primitives, vitest + Testing Library (jsdom), `api.invoke` / `api.listen` from `src/api/`.

**Spec:** `docs/superpowers/specs/2026-10-01-collab-publish-review-design.md` (§5.4, §6.3–§6.5, §7, §8, §9, §11, §16 wave-1 amendments). Visual reference: the canvas https://claude.ai/artifact/Eqn7HKZktqD3VfRpCqwE6z. A copy of its artboards is in `docs/superpowers/research/2026-10-01-collab-publish-review-mockup/` (`Main.dc.html`, `MyFrames.dc.html`, `Blink*.dc.html`). Copy, labels and layout come from there.

## Global Constraints

- Branch `collab-publish-review` (wave 1 head `51b64ad7`). Push nothing; merge only on the owner's word.
- **No `@tauri-apps/*` outside `src/api/`.** Every backend call goes through `api.invoke` / `api.listen`.
- **Listener pattern (CLAUDE.md, StrictMode-safe)**:
  ```ts
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<T>('event', (p) => { if (cancelled) return; handle(p); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[X] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [deps]);
  ```
- **Notifications** only through `notify()` from `useNotifications()`, and only on discrete outcomes, never on `*-progress`. Links use `/projects/${id}?tab=…` (the `segment` param is added in Task 2). Every failure is logged with `console.error` before it is notified.
- **Design tokens only** (`bg-surface`, `text-content-muted`, `text-accent`, `text-success`, `text-warning`, `text-error`, `border-line`, …), never raw colours. Use the existing `src/components/ui/` primitives:
  - `Card`, `Button`, `Chip`, `Pill`, `StatusDot`, `Seg`, `SegmentTiles`, `ProgressBar`, `EmptyState`, `DialogShell`, `KV`;
  - `Checkbox` from `src/components/settings/Checkbox.tsx` — a NAMED export, `{ checked, onChange(checked), label, disabled, role: 'checkbox' | 'switch' }`; its `label` is the accessible name.
  - `Chip` tones are exactly `ok | warn | err | info | mute` (Task 5 adds `pur`, the mockup's `c-pur`). `Card` takes `title / subtitle / action / className / flush`, with no `aria-label`. `Seg` is `{ options: { value, label }[], value, onChange }` and marks the chosen button `aria-pressed`.
- **Wire names:**
  - `set_project_publish_mode { projectId, mode }`
  - `calibrate_collab_frames { projectId, frameIds }`
  - `publish_collab_frames { projectId, frameIds }`
  - `set_collab_frames_withheld { projectId, frameIds, withheld } → number`
  - `get_collab_publish_run { projectId } → CollabPublishRunView`
  - `cancel_collab_publish { projectId }`
  - `get_collab_blink_frames { projectId, refs: CollabFrameRef[] } → CollabBlinkEntry[]`
  - `get_collab_frame_image { projectId, frame: CollabFrameRef, resolution? } → JPEG bytes` (the key is **`frame`**)
  - events `collab-publish-progress`, `collab-publish-finished`, `collab-project-synced`
  - `collab-published` is retired.
- **Types:** use the generated types in `src/types/models.ts`, never hand-written copies:
  - `PublishMode = "manual" | "autoCalibrate" | "automatic"`
  - `ProjectCard.publishMode` and `syncedAt: string | null`
  - `OwnFrameRow.segment` (`"ready" | "review" | "published" | "held"`), `calibratedPath`, `calibratedBytes`, `preparedAt`, `withheld`
  - `ContributorCounts.prepared` / `.withheld`
  - `CollabPublishProgress` / `CollabPublishFinished` / `CollabPublishRunView`
  - `PublishStage` / `PublishOutcome` / `PublishRunKind`
  - `CollabProjectSynced`, `CollabFrameRef` (both keys present, `null` allowed), `CollabBlinkEntry`, `BlinkSource`
- **Dates:** `formatTimestamp(iso, { seconds: true })` for every shown timestamp (owner rule: `YYYY-MM-DD HH:MM:SS`). Durations use `formatDurationPadded`, sizes `formatSize`.
- **Actions** follow the eligible-subset label convention (`Verb N`, `Verb N of M`, `Verb all N`), which `ProjectFrameTable` already implements.
- **Tests:**
  - Iterate with `npx vitest run <file>`.
  - The task gate is `npx tsc --noEmit -p . && npx vitest run <files>`.
  - The final task runs the whole suite (`npx vitest run`) and `npx tsc --noEmit -p .`.
  - Mock `api` exactly as the neighbouring tests do (`vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }))`).
- No third-party project names anywhere (code, comments, commit messages).
- Commits as the user, each message ending with:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_015RRpMcroNShaEgp8Q33fR3
  ```

## Plan rulings (recorded in spec §16 by Task 9)

| # | Ruling |
| ---- | ---- |
| F1 | **Update** (published frames with `contributorState === "updatePending"`) runs `publish_collab_frames` with those ids directly, with no confirm. The frames are already in the project, and the run panel shows the work. |
| F2 | **Don't publish** asks for confirmation only when the targets include prepared (To review) frames, because their calibrated files are deleted. From Ready it acts at once. |
| F3 | The run panel's step strip is **monotonic per run**. An `auto` run re-entering `queued` / `calibrating` for its update step (a wave-1 leftover) never moves a finished step back. |
| F4 | A refusal returned by a command before a run starts (busy; A6 from the cached binding; outdated hub) stays where it is today: inline line, `updateRequired` banner, A6 refusal box, busy info toast. Every outcome of a started run is notified **only** from `collab-publish-finished` (§5.4). `usePublishing` stops toasting run failures. |
| F5 | The Live pill shows the newest of the card's `syncedAt` and every `collab-project-synced.syncedAt` it hears for its project, so the age restarts on each confirmation without a card re-read. |
| F6 | Unlinking a set while a run is active returns the busy text from core. `LinkObjectDialog` shows "A publish run is in progress — try again when it ends." for that error. |
| F7 | Blink's canvas height is measured from the real header (toolbar plus context strip) with a `ResizeObserver`, replacing the hard-coded `window.innerHeight - 48`. |
| F8 | A run that calibrated frames and sent none (a calibrate run, or an `auto` run in Auto-calibrate mode) notifies "Calibrated n frames in {title} — review them" and links To review. The spec's "calibrate done" row covers both kinds: in Auto-calibrate, the review notice is the one sign that work was done. A `refused` outcome whose error is an A6 refusal (`collab_publishing_device:<name>`) reads "{device} publishes {title}", never the raw code. |
| F9 | The run panel's step strip follows the run kind: calibrate = Queued · Calibrate; publish = Seed · Announce; republish and auto = Queued · Calibrate · Seed · Announce. `versions` lights Announce. A publish run that regenerates an update (`queued` / `calibrating`) shows no lit step, and its title carries the stage. |

## Review Focus

1. **A run already in progress when the page opens.** The run panel and the settings status show it at once, from `get_collab_publish_run`, not after the next event. Pinned in Task 1 (`a_run_in_progress_on_mount_is_shown_from_the_snapshot`).
2. **The Live pill while the hub refuses the project.** A `collab-project-synced { ok: false }` stops "Syncing…" at once and tells why. The pill never spins for 30 s. Pinned in Task 6 (`a_not_ok_report_stops_the_pill_and_notifies`).
3. **Typing a reason in the Exclude dialog opened over Blink.** No Blink shortcut fires, and Escape closes only the dialog. Pinned in Task 7 (`keys_typed_in_a_dialog_over_blink_do_not_drive_blink`).
4. **Don't publish on a mixed selection** (prepared plus Ready frames, from the To review segment or Blink). The confirmation names how many calibrated files will be deleted, and the right frames move to Held back. Pinned in Task 5 (`dont_publish_on_review_frames_confirms_and_names_the_files`).
5. **Opening Blink from a table where some selected frames are not on this device.** The label says `Blink N of M`; only the eligible frames open; the backend's dropped refs do not break the viewer. Pinned in Task 8 (`blink_opens_only_frames_held_here`).

---

### Task 0: Baseline

- [ ] **Step 1:** `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum && git status --short && git branch --show-current` → clean, `collab-publish-review`.
- [ ] **Step 2:** `npx tsc --noEmit -p . 2>&1 | grep -c "error TS"` → expect **16** errors, all from the regenerated `models.ts`. Then `npx vitest run 2>&1 | tail -5` → record the pass count; vitest does not type-check, so it passes. Record both numbers in the report.

---

### Task 1: `useCollabPublishRun` — the run snapshot and its events

**Files:**
- Create: `src/components/collab/project/useCollabPublishRun.ts`
- Create: `src/components/collab/project/useCollabPublishRun.test.tsx`

**Interfaces:**
- Produces:
  ```ts
  export interface PublishRunState {
    running: CollabPublishProgress | null;
    last: CollabPublishFinished | null;
    /** Highest step index reached by the running run (plan F3); -1 when idle. */
    reached: number;
    cancel: () => Promise<void>;
    cancelBusy: boolean;
  }
  export function useCollabPublishRun(projectId: string | undefined): PublishRunState;
  export const STEP_ORDER: PublishStage[]; // ['queued','calibrating','seeding','announcing','versions']
  export function stepIndex(stage: PublishStage): number;
  ```

- [ ] **Step 1: Write the failing tests** (`useCollabPublishRun.test.tsx`):

```tsx
import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import type { CollabPublishFinished, CollabPublishProgress, CollabPublishRunView } from '../../../types/models';
import { useCollabPublishRun } from './useCollabPublishRun';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const listeners: Record<string, (p: unknown) => void> = {};
const progress = (o: Partial<CollabPublishProgress> = {}): CollabPublishProgress => ({
  projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual', mode: null,
  stage: 'calibrating', current: 3, total: 10, currentFile: 'c_a.fits', startedAt: '2026-10-02T10:00:00Z', ...o,
});
const finished = (o: Partial<CollabPublishFinished> = {}): CollabPublishFinished => ({
  projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual', outcome: 'done',
  calibrated: 10, announced: 0, updated: 0, stale: 0, heldBack: 0, error: null,
  startedAt: '2026-10-02T10:00:00Z', finishedAt: '2026-10-02T10:05:00Z', ...o,
});
const wrapper = ({ children }: { children: React.ReactNode }) => <NotificationProvider>{children}</NotificationProvider>;

beforeEach(() => {
  vi.mocked(api.listen).mockImplementation(((name: string, cb: (p: unknown) => void) => {
    listeners[name] = cb;
    return Promise.resolve(() => { delete listeners[name]; });
  }) as typeof api.listen);
});

describe('useCollabPublishRun', () => {
  it('a_run_in_progress_on_mount_is_shown_from_the_snapshot', async () => {
    const view: CollabPublishRunView = { running: progress(), last: null };
    vi.mocked(api.invoke).mockResolvedValueOnce(view);
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(result.current.running?.current).toBe(3));
    expect(api.invoke).toHaveBeenCalledWith('get_collab_publish_run', { projectId: 'p1' });
  });

  it('follows progress and finished events for its project only', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: null, last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(listeners['collab-publish-progress']).toBeDefined());
    act(() => listeners['collab-publish-progress'](progress({ projectId: 'other' })));
    expect(result.current.running).toBeNull();
    act(() => listeners['collab-publish-progress'](progress({ current: 7 })));
    expect(result.current.running?.current).toBe(7);
    act(() => listeners['collab-publish-finished'](finished()));
    expect(result.current.running).toBeNull();
    expect(result.current.last?.calibrated).toBe(10);
  });

  it('the reached step never moves back within one run (F3)', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: null, last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(listeners['collab-publish-progress']).toBeDefined());
    act(() => listeners['collab-publish-progress'](progress({ kind: 'auto', stage: 'announcing' })));
    act(() => listeners['collab-publish-progress'](progress({ kind: 'auto', stage: 'calibrating' })));
    expect(result.current.reached).toBe(3); // announcing
    act(() => listeners['collab-publish-progress'](progress({ publishRunId: 'r2', stage: 'queued' })));
    expect(result.current.reached).toBe(0); // a new run starts over
  });

  it('cancel invokes cancel_collab_publish and logs + notifies a failure', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: progress(), last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(result.current.running).not.toBeNull());
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('boom'));
    await act(async () => { await result.current.cancel(); });
    expect(api.invoke).toHaveBeenLastCalledWith('cancel_collab_publish', { projectId: 'p1' });
    expect(err).toHaveBeenCalled();
  });

  it('a failed snapshot read is logged and leaves an idle state', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db'));
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(err).toHaveBeenCalled());
    expect(result.current.running).toBeNull();
  });
});
```

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/project/useCollabPublishRun.test.tsx` → module not found.

- [ ] **Step 3: Implement** `useCollabPublishRun.ts`:

```ts
import { useCallback, useEffect, useState } from 'react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import type {
  CollabPublishFinished,
  CollabPublishProgress,
  CollabPublishRunView,
  PublishStage,
} from '../../../types/models';

/** Spec 2026-10-01 §5: the order a run's stages can reach. */
export const STEP_ORDER: PublishStage[] = ['queued', 'calibrating', 'seeding', 'announcing', 'versions'];
export function stepIndex(stage: PublishStage): number {
  return STEP_ORDER.indexOf(stage);
}

export interface PublishRunState {
  running: CollabPublishProgress | null;
  last: CollabPublishFinished | null;
  /** Highest step reached by the running run (plan F3); -1 when idle. */
  reached: number;
  cancel: () => Promise<void>;
  cancelBusy: boolean;
}

/**
 * The project's publish-family run: the snapshot on mount (a page opened
 * mid-run shows it at once), then `collab-publish-progress` /
 * `collab-publish-finished` for this project. One source for the My frames
 * run panel and the Project settings status.
 */
export function useCollabPublishRun(projectId: string | undefined): PublishRunState {
  const { notify } = useNotifications();
  const [running, setRunning] = useState<CollabPublishProgress | null>(null);
  const [last, setLast] = useState<CollabPublishFinished | null>(null);
  const [reached, setReached] = useState(-1);
  const [cancelBusy, setCancelBusy] = useState(false);

  const applyProgress = useCallback((p: CollabPublishProgress) => {
    setRunning((prev) => {
      const sameRun = prev?.publishRunId === p.publishRunId;
      setReached((r) => (sameRun ? Math.max(r, stepIndex(p.stage)) : stepIndex(p.stage)));
      return p;
    });
  }, []);

  useEffect(() => {
    if (!projectId) return undefined;
    let cancelled = false;
    api
      .invoke<CollabPublishRunView>('get_collab_publish_run', { projectId })
      .then((v) => {
        if (cancelled || !v) return;
        if (v.running) applyProgress(v.running);
        setLast(v.last);
      })
      .catch((err) => console.error('[publish-run] snapshot failed:', err));
    return () => {
      cancelled = true;
    };
  }, [projectId, applyProgress]);

  useEffect(() => {
    if (!projectId) return undefined;
    let cancelled = false;
    const offs: Array<() => void> = [];
    const sub = <T,>(name: string, handle: (p: T) => void) =>
      api
        .listen<T>(name, (p) => {
          if (!cancelled) handle(p);
        })
        .then((fn) => {
          if (cancelled) fn();
          else offs.push(fn);
        })
        .catch((err) => console.error(`[publish-run] listen ${name} failed:`, err));
    void sub<CollabPublishProgress>('collab-publish-progress', (p) => {
      if (p.projectId === projectId) applyProgress(p);
    });
    void sub<CollabPublishFinished>('collab-publish-finished', (f) => {
      if (f.projectId !== projectId) return;
      setRunning(null);
      setReached(-1);
      setLast(f);
    });
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, [projectId, applyProgress]);

  const cancel = useCallback(async () => {
    if (!projectId) return;
    setCancelBusy(true);
    try {
      await api.invoke('cancel_collab_publish', { projectId });
    } catch (err) {
      console.error('[publish-run] cancel failed:', err);
      notify({
        title: 'Could not cancel the run',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setCancelBusy(false);
    }
  }, [projectId, notify]);

  return { running, last, reached, cancel, cancelBusy };
}
```

`setReached` inside `setRunning`'s updater keeps the "same run" comparison against the previous value without a stale closure. If a lint rule forbids side effects in updaters, keep a `lastRunIdRef` and compute both in `applyProgress` instead, with the same behaviour.

- [ ] **Step 4: Run:** `npx vitest run src/components/collab/project/useCollabPublishRun.test.tsx` → green.
- [ ] **Step 5: Commit** `feat(collab-ui): useCollabPublishRun — run snapshot, progress, finished, cancel` (trailers as in Global Constraints).

---

### Task 2: Model plumbing — review segment, held kinds, notifications, deep link, type repair

**Files:**
- Modify: `src/components/collab/project/MyFramesTab.tsx` (`Segment` :17)
- Modify: `src/components/collab/project/attention.ts` (`AttentionTarget` :8-14, `CAUSE` :39-67)
- Modify: `src/components/collab/project/frames.tsx`:
  - `TableId` :14
  - `fromOwn` :71-127
  - `REASON_LABEL` :285-295
  - `TABLES` :750-815 (add `review`; the held facet options)
- Modify: `src/components/collab/contributorState.ts` (`SHORT`)
- Modify: `src/components/collab/FrameSetProjectBlock.tsx` (`summary()` :8-20)
- Modify: `src/components/collab/project/FramePanel.tsx`:
  - own status chip :150-154
  - "On this device" :250-256 (adds the calibrated file line)
- Modify: `src/components/collab/project/usePublishing.ts` (F4: no toast for run failures or A6; add `calibrate`)
- Modify: `src/hooks/useCollabNotifications.ts` (`collab-published` :262-294 → `collab-publish-finished` per spec §5.4)
- Modify: `src/pages/ProjectDetail.tsx`:
  - `collab-published` listener :228-248 → `collab-publish-finished`
  - `?segment=` deep link :159-169
  - facet setters :152-155 + :517 + :645 add `review`
  - tab badge
- Modify: `src/components/collab/project/MetaLine.tsx` + test. Remove only the auto-publish toggle so the file compiles; Task 4 deletes the file.
- Fix fixtures (add the four `OwnFrameRow` fields, `publishMode`, `syncedAt`, `ContributorCounts.prepared/withheld`, `PublishResult.calibrated/stale`):
  - `FramePanel.test.tsx:14`
  - `frames.test.tsx:7`
  - `MyFramesTab.test.tsx:18`
  - `OverviewTab.test.tsx:16`
  - `ReasonGroupAction.test.tsx:12`
  - `table/ProjectFrameTable.test.tsx:11`
  - `pages/ProjectDetail.test.tsx:47,68,181`
  - `FrameSetProjectBlock.test.tsx:18-22`
  - `hooks/useCollabNotifications.test.tsx:47`

**Interfaces:**
- Produces:
  - `export type Segment = 'ready' | 'review' | 'published' | 'held';`
  - `AttentionTarget` `segment` union including `'review'`.
  - `TableId` including `'review'`.
  - `HELD_KIND_ORDER = [...BLOCKER_ORDER, 'withheld', 'blackHole']` (in `frames.tsx`), and `REASON_LABEL.withheld = 'Withheld by you'`, `REASON_LABEL.blackHole = 'In the Black Hole'`.
  - `usePublishing(...).calibrate(ids: number[]): Promise<void>`, with `calibrateBusy` and `calibrateError`.
  - `ProjectDetail` accepts `?segment=<ready|review|published|held>` alongside `?tab=`.

- [ ] **Step 1: Write the failing tests.**

`frames.test.tsx` (add):
```tsx
it('a review row has no states and a withheld held row carries the withheld kind', () => {
  const review = fromOwn(own({ segment: 'review', calibratedPath: '/c/c_a.fits', calibratedBytes: 64 }));
  expect(review.states).toEqual([]);
  const w = fromOwn(own({ segment: 'held', withheld: true, failures: [{ kind: 'withheld', text: 'Withheld by you' }] }));
  expect(w.states).toEqual(['withheld']);
  expect(REASON_LABEL.withheld).toBe('Withheld by you');
  expect(REASON_LABEL.blackHole).toBe('In the Black Hole');
});
```

`attention.test.ts` (add):
```ts
it('withheld and blackHole held kinds get their own items, never the unknown-kind error', () => {
  const err = vi.spyOn(console, 'error').mockImplementation(() => {});
  const items = deriveAttention({
    own: [
      own({ segment: 'held', failures: [{ kind: 'withheld', text: 'Withheld by you' }] }),
      own({ segment: 'held', failures: [{ kind: 'blackHole', text: 'In the Black Hole' }] }),
    ],
    library: [], members: [], canModerate: false, canReceive: false, liveRunning: true, pending: 0, now: 0,
  });
  expect(items.map((i) => i.target)).toEqual(
    expect.arrayContaining([
      { kind: 'segment', segment: 'held', state: 'withheld' },
      { kind: 'segment', segment: 'held', state: 'blackHole' },
    ]),
  );
  expect(err).not.toHaveBeenCalled();
});
```

`useCollabNotifications.test.tsx` (add, using the file's `emit` / `renderHarness` / `settle` helpers). Each case emits a `collab-publish-finished` payload:

| Payload | Expected notify |
| ---- | ---- |
| calibrate done `{calibrated: 46}` | title `Calibrated 46 frames in <title> — review them`, link `/projects/p1?tab=mine&segment=review` |
| auto done `{calibrated: 5}`, nothing sent (F8) | title `Calibrated 5 frames in <title> — review them` |
| refused + error `collab_publishing_device:Obs PC` (F8) | title `Obs PC publishes <title>` |
| publish done `{announced: 3, updated: 1}` | `Published 4 frames in <title>`, link `…&segment=published` |
| publish done, nothing sent, `heldBack: 2` | warning `Nothing new to publish in <title>`, link `…&segment=held` |
| `cancelled` | info, `toast: false`, title `Stopped in <title>` |
| `refused` + trigger auto | `toast: false` |
| `refused` + trigger manual | toast |
| `failed` + error `"disk full"` | `hasErrors: true`, detail contains `disk full` |
| done with all counts 0 | no notify |

`ProjectDetail.test.tsx`:
- Rewrite the four `collab-published` tests (:332, :1131, :1148, :1374) to fire `collab-publish-finished` with the same intent: the page reloads own frames, library, detail and members.
- Add `it('?tab=mine&segment=review opens My frames on To review and cleans the URL')`.

`usePublishing` (in `ProjectDetail.test.tsx` or a new `usePublishing.test.tsx`):
- A generic `publish_collab_frames` failure shows the inline error and **no** toast (F4).
- `calibrate([1, 2])` invokes `calibrate_collab_frames { projectId, frameIds: [1, 2] }`.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/project/frames.test.tsx src/components/collab/project/attention.test.ts src/hooks/useCollabNotifications.test.tsx src/pages/ProjectDetail.test.tsx`.

- [ ] **Step 3: Implement.**

**`frames.tsx`:**
- `TableId` gains `'review'`.
- `fromOwn` keeps today's logic. The `held` branch already takes distinct failure kinds, so `withheld` / `blackHole` flow through. The `review` branch returns `[]`.
- Add:

```ts
/** Held-back facet order: the gate's blocker kinds, then the two local states (spec §4.1). */
export const HELD_KIND_ORDER = [...BLOCKER_ORDER, 'withheld', 'blackHole'] as const;
```

  `REASON_LABEL` gains `withheld: 'Withheld by you'` and `blackHole: 'In the Black Hole'`. The `held` table's `stateFacet.options` uses `HELD_KIND_ORDER.map((k) => [k, REASON_LABEL[k]])`.
- `TABLES.review = { …same columns as ready…, stateFacet: null, defaultGroup: ['night', 'filter'] }`. Copy the `ready` entry and change only the id and empty texts.
- Leave `table/model.ts` `BLOCKER_ORDER` unchanged: it mirrors `gate.rs`, where these two are not blockers.

**`attention.ts`:**
- `AttentionTarget.segment` gains `'review'`.
- `CAUSE` gains:

```ts
  withheld: {
    title: (n) => `${n} ${n === 1 ? 'frame' : 'frames'} withheld by you`,
    detail: 'They are never published. Release them to publish them.',
    action: 'Review',
  },
  blackHole: {
    title: (n) => `${n} ${n === 1 ? 'frame is' : 'frames are'} in the Black Hole`,
    detail: 'Restore them from the Black Hole to publish them.',
    action: 'Review',
  },
```

  Match the exact shape of the existing `CAUSE` entries (`title(n, rows)`), and do not change the order of the existing items.

**`contributorState.ts`:** `SHORT.prepared = 'to review'`, `SHORT.withheld = 'withheld'`.

**`FrameSetProjectBlock.tsx` `summary()`:** insert `if (c.prepared) parts.push(`${c.prepared} to review`);` right after published, and `if (c.withheld) parts.push(`${c.withheld} withheld`);` after "fail gate".

**`FramePanel.tsx`:**
- The own status chip maps `review` → `Chip tone="info"` "to review".
- Under "On this device", when `frame.own?.calibratedPath` is set, add a second line labelled "Calibrated" with that path, using the same mono style and the existing copy button pattern.

**`usePublishing.ts` (F4):**
- In the `publish` / `republish` catch-all branch, remove the `notify(...)` for "Publish failed" / "Republish failed". Keep `console.error` and the inline error state.
- Keep the busy info toast: there is no run, so no finished event follows.
- Keep `updateRequired`.
- A6 (`collab_publishing_device:`): keep `setRefusedBy` and the detail reload, and drop the toast. The finished event notifies.
- Export the existing `publishingDeviceRefusal(msg)` (Task 2's notifications and Task 4's run wording use it).
- Add `calibrate(ids)`. It mirrors `publish` but calls `calibrate_collab_frames`, with its own `calibrateBusy` / `calibrateError` state and the same busy / A6 / outdated handling. It does not open a confirm.

**`useCollabNotifications.ts`:** replace the `collab-published` subscription with `collab-publish-finished`, typed as `CollabPublishFinished`. Map it per spec §5.4:

```ts
const sent = f.announced + f.updated;
const base = `/projects/${f.projectId}?tab=mine`;
if (f.outcome === 'done' && f.calibrated + sent + f.heldBack + f.stale === 0) return;
switch (f.outcome) {
  case 'done':
    if (sent === 0 && f.calibrated > 0) {
      notify({ title: `Calibrated ${f.calibrated} frames in ${title} — review them`,
        detail: f.heldBack > 0 ? `${f.heldBack} held back` : undefined,
        kind: 'project', tone: 'success', hasErrors: f.heldBack > 0, link: `${base}&segment=review` });
    } else if (sent > 0) {
      notify({ title: `Published ${sent} frames in ${title}`,
        detail: `${f.announced} new · ${f.updated} updated${f.heldBack > 0 ? ` · ${f.heldBack} held back` : ''}`,
        kind: 'project', tone: 'success', hasErrors: f.heldBack > 0, link: `${base}&segment=published` });
    } else if (f.heldBack > 0) {
      notify({ title: `Nothing new to publish in ${title}`, detail: `${f.heldBack} held back`,
        kind: 'project', tone: 'warning', link: `${base}&segment=held` });
    }
    break;
  case 'cancelled':
    notify({ title: `Stopped in ${title}`, kind: 'project', tone: 'info', toast: false, link: base });
    break;
  case 'refused': {
    const device = f.error ? publishingDeviceRefusal(f.error) : null;
    notify({
      title: device ? `${device} publishes ${title}` : `Not published in ${title}`,
      detail: device ? 'Use Publish from this device in Project settings to take over.' : (f.error ?? undefined),
      kind: 'project', tone: 'warning', toast: f.trigger === 'manual', link: base });
    break;
  }
  case 'failed':
    console.error('[collab] publish run failed:', f.error);
    notify({ title: `Publishing failed in ${title}`, detail: f.error ?? undefined,
      kind: 'project', tone: 'warning', hasErrors: true, link: base });
    break;
}
```

Use the file's existing title lookup and `NotificationKind`. Notification tones are `info | warning | success`. Remove the hand-declared `CollabPublishedEvent`.

**`ProjectDetail.tsx`:**
- The reload listener subscribes to `collab-publish-finished` with the same body.
- The deep-link effect also reads `segment`. When it is one of the four segments, it calls `setSegment` before `setTab('mine')`, then deletes both params with `replace: true`.
- The facet setter maps at :517 / :645 gain `review: setReviewFacets`. Add `useSessionState<Facets>(`collab.${id}.review.facets`, EMPTY_FACETS)`.
- The My frames badge becomes `readyCount > 0 || reviewCount > 0 ? `${readyCount} ready · ${reviewCount} to review` : ''`. Keep the existing `b.n > 0` gate by setting `n = readyCount + reviewCount`.

**`MetaLine.tsx`:** delete the "Auto-publish on/off" button and its `flip('publish')` branch, and delete the tests asserting it. Keep the rest compiling; Task 4 deletes the file.

**Fixtures:** add the missing fields with neutral values: `calibratedPath: null, calibratedBytes: null, preparedAt: null, withheld: false`, `publishMode: 'manual'`, `syncedAt: null`, `prepared: 0, withheld: 0`, `calibrated: 0, stale: 0`.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p .` → **0 errors**. Then `npx vitest run` (full) → green.
- [ ] **Step 5: Commit** `feat(collab-ui): review segment, withheld and Black Hole kinds, run outcome notifications, segment deep link`.

---

### Task 3: Overview — My contribution first, four detailed tiles, a settings slot

**Files:**
- Create: `src/components/collab/project/contribution.ts` and `contribution.test.ts`
- Modify: `src/components/collab/project/OverviewTab.tsx` (layout :115; My contribution :84-111 / :184-197; add the `settings` prop)
- Modify: `src/components/collab/project/attention.ts` (a To review item)
- Modify: `src/components/collab/project/OverviewTab.test.tsx`

**Interfaces:**
- Consumes: Task 2's `Segment` (with `'review'`), `HELD_KIND_ORDER`, `REASON_LABEL`.
- Produces:
  ```ts
  export interface ContributionTile {
    segment: Segment;            // 'ready' | 'review' | 'published' | 'held'
    count: number;
    seconds: number;             // Σ exptimeSec (null rows add 0)
    nights: number;              // distinct non-null `night`
    bytes: number;               // raw byteSize for ready/held, calibratedBytes ?? byteSize for review/published
    filters: { filter: string; seconds: number }[]; // filterOrder
    footer: string;              // §7.2 footer text
  }
  export function contributionTiles(own: OwnFrameRow[]): ContributionTile[]; // always 4, in segment order
  ```
  `OverviewTab` gains `settings?: ReactNode`, rendered first in the right column. Task 4 passes the Project settings card.

- [ ] **Step 1: Write the failing tests** (`contribution.test.ts`):

```ts
import { describe, expect, it } from 'vitest';
import type { OwnFrameRow } from '../../../types/models';
import { contributionTiles } from './contribution';

const row = (o: Partial<OwnFrameRow>): OwnFrameRow => ({
  frameId: 1, frameUuid: null, fileName: 'a.fits', setId: 1, setName: 'S', night: '2026-09-28', filter: 'L',
  filterMapped: true, camera: 'C', exptimeSec: 300, byteSize: 100, fwhmArcsec: null, eccentricity: null,
  starsDetected: null, medianSnr: null, segment: 'ready', contributorState: 'notPublished', contributorReason: null,
  failures: [], contentVersion: null, pubState: null, acceptedReason: null, holdersOnline: null, holdersTotal: null,
  localState: null, publishedAt: null, lastError: null, rules: [], path: '/d/a.fits', accepted: null,
  calibratedPath: null, calibratedBytes: null, preparedAt: null, withheld: false, ...o,
});

describe('contributionTiles', () => {
  it('sums hours, nights, filters and size per segment, in segment order', () => {
    const t = contributionTiles([
      row({ segment: 'ready', filter: 'L', night: '2026-09-28' }),
      row({ segment: 'ready', filter: 'R', night: '2026-09-29', exptimeSec: 600 }),
      row({ segment: 'review', filter: 'Ha', calibratedBytes: 400 }),
      row({ segment: 'published', accepted: true, pubState: 'published', calibratedBytes: 500 }),
      row({ segment: 'published', pubState: 'pending', calibratedBytes: null, byteSize: 50 }),
    ]);
    expect(t.map((x) => x.segment)).toEqual(['ready', 'review', 'published', 'held']);
    const [ready, review, published, held] = t;
    expect([ready.count, ready.seconds, ready.nights, ready.bytes]).toEqual([2, 900, 2, 200]);
    expect(ready.filters.map((f) => f.filter)).toEqual(['L', 'R']);
    expect(review.bytes).toBe(400);
    expect(published.bytes).toBe(550);
    expect(published.footer).toBe('1 accepted · 1 pending');
    expect(held.count).toBe(0);
  });

  it('held back lists the top three reasons with counts', () => {
    const t = contributionTiles([
      row({ segment: 'held', failures: [{ kind: 'threshold', text: 'FWHM' }] }),
      row({ segment: 'held', failures: [{ kind: 'threshold', text: 'FWHM' }] }),
      row({ segment: 'held', failures: [{ kind: 'withheld', text: 'Withheld by you' }] }),
      row({ segment: 'held', failures: [{ kind: 'analyze', text: 'no analysis' }] }),
      row({ segment: 'held', failures: [{ kind: 'blackHole', text: 'In the Black Hole' }] }),
    ]);
    expect(t[3].footer).toBe('2 quality thresholds · 1 no analysis · 1 withheld by you');
  });

  it('a null exptime counts the frame but adds no time', () => {
    const t = contributionTiles([row({ exptimeSec: null })]);
    expect([t[0].count, t[0].seconds]).toEqual([1, 0]);
  });
});
```

Add to `OverviewTab.test.tsx`:
```tsx
it('renders My contribution above Integration in the left column and the settings slot first on the right', () => {
  renderTab({ settings: <section aria-label="Project settings">S</section> });
  const left = screen.getByRole('heading', { name: 'My contribution' }).closest('[data-col="left"]');
  expect(left).not.toBeNull();
  const headings = within(left as HTMLElement).getAllByRole('heading').map((h) => h.textContent);
  expect(headings.slice(0, 2)).toEqual(['My contribution', 'Integration toward goal']);
  const right = screen.getByLabelText('Project settings').closest('[data-col="right"]');
  expect(right?.firstElementChild?.getAttribute('aria-label')).toBe('Project settings');
});

it('each contribution tile shows hours, nights, size and per-filter hours', () => {
  renderTab({ own: [ownRow({ segment: 'ready', exptimeSec: 3600, filter: 'L' })] });
  const tile = screen.getByRole('button', { name: /ready to calibrate/i });
  expect(tile).toHaveTextContent('1h 00m');
  expect(tile).toHaveTextContent('1 night');
  expect(tile).toHaveTextContent('L');
});

it('the To review tile opens the review segment', async () => {
  const onOpenSegment = vi.fn();
  renderTab({ onOpenSegment, own: [ownRow({ segment: 'review' })] });
  await userEvent.click(screen.getByRole('button', { name: /to review/i }));
  expect(onOpenSegment).toHaveBeenCalledWith('review');
});
```

Adapt `ownRow`, `renderTab` and the user-event import to the file's existing helpers. Rewrite the old three-tile tests ("clicking the ready tile…", "renders Published N and Held back N…") to the four tiles.

In `attention.test.ts`, add `it('N prepared frames add a To review item first')`: two `review` rows produce the first item with `target { kind: 'segment', segment: 'review' }` and title `2 calibrated frames wait for your review`.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/project/contribution.test.ts src/components/collab/project/OverviewTab.test.tsx src/components/collab/project/attention.test.ts`.

- [ ] **Step 3: Implement.**

`contribution.ts`:

```ts
import type { OwnFrameRow } from '../../../types/models';
import type { Segment } from './MyFramesTab';
import { filterOrder } from './table/model';
import { REASON_LABEL } from './frames';

export interface ContributionTile {
  segment: Segment;
  count: number;
  seconds: number;
  nights: number;
  bytes: number;
  filters: { filter: string; seconds: number }[];
  footer: string;
}

const ORDER: Segment[] = ['ready', 'review', 'published', 'held'];

/** Spec 2026-10-01 §7.2 — the four My contribution tiles, derived from own rows. */
export function contributionTiles(own: OwnFrameRow[]): ContributionTile[] {
  return ORDER.map((segment) => {
    const rows = own.filter((r) => r.segment === segment);
    const seconds = rows.reduce((s, r) => s + (r.exptimeSec ?? 0), 0);
    const nights = new Set(rows.map((r) => r.night).filter((n): n is string => !!n)).size;
    const calibrated = segment === 'review' || segment === 'published';
    const bytes = rows.reduce((s, r) => s + (calibrated ? (r.calibratedBytes ?? r.byteSize) : r.byteSize), 0);
    const perFilter = new Map<string, number>();
    for (const r of rows) perFilter.set(r.filter, (perFilter.get(r.filter) ?? 0) + (r.exptimeSec ?? 0));
    const filters = [...perFilter.entries()]
      .map(([filter, s]) => ({ filter, seconds: s }))
      .sort((a, b) => filterOrder(a.filter, b.filter));
    return { segment, count: rows.length, seconds, nights, bytes, filters, footer: footerOf(segment, rows) };
  });
}

function footerOf(segment: Segment, rows: OwnFrameRow[]): string {
  switch (segment) {
    case 'ready':
      return 'Calibrate →';
    case 'review':
      return 'Review and publish →';
    case 'published': {
      const accepted = rows.filter((r) => r.accepted === true).length;
      const pending = rows.filter((r) => r.pubState === 'pending').length;
      return `${accepted} accepted · ${pending} pending`;
    }
    case 'held': {
      const byKind = new Map<string, number>();
      for (const r of rows) {
        const k = r.failures[0]?.kind ?? 'threshold';
        byKind.set(k, (byKind.get(k) ?? 0) + 1);
      }
      return [...byKind.entries()]
        .sort((a, b) => b[1] - a[1])
        .slice(0, 3)
        .map(([k, n]) => `${n} ${(REASON_LABEL[k as keyof typeof REASON_LABEL] ?? k).toLowerCase()}`)
        .join(' · ');
    }
  }
}
```

Check the exact names and signatures of `filterOrder` (a comparator over filter names in `table/model.ts`) and `REASON_LABEL`'s key type; adapt the imports, not the behaviour. If `REASON_LABEL.threshold` is "Quality thresholds", the test's footer reads `2 quality thresholds`; keep the test and the label in agreement.

`OverviewTab.tsx`:
- The outer grid stays `grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)] items-start gap-3.5 max-[900px]:grid-cols-1`.
- Left column: `<div data-col="left" className="grid gap-3.5">` holding **My contribution**, then **Integration toward goal**.
- Right column: `<div data-col="right" className="grid gap-3.5">` holding `{settings}`, then Needs attention, Exchange now, Quality thresholds.
- My contribution becomes `grid grid-cols-4 gap-2 max-[900px]:grid-cols-2`, one `<button type="button">` per tile. Each tile contains:
  - the count (`text-[22px] font-semibold`, tone per segment: ready `text-accent`, review `text-purple`, published `text-success`, held `text-warning`);
  - the label (`ready to calibrate`, `to review`, `published`, `held back`);
  - the meta line `formatDurationPadded(seconds) · N night(s) · formatSize(bytes)`;
  - the per-filter row (`FilterDot` + filter + `formatDurationPadded`);
  - the footer, after a top border.

  The button's accessible name must start with the label (`aria-label={`${count} ${label}`}`), so `getByRole('button', { name: /to review/i })` matches. If the theme has no purple token, use the existing token the Chip `info` tone uses and note it.
- The empty and loading states are unchanged (`Loading…` / `Not available.`).

`attention.ts`: before the held items, if `own` has `n > 0` rows with `segment === 'review'`, push `{ key: 'review', tone: 'warn', count: n, title: `${n} calibrated ${n === 1 ? 'frame waits' : 'frames wait'} for your review`, detail: 'Blink them, drop the bad ones, then publish.', action: 'Review', target: { kind: 'segment', segment: 'review' } }`. If `tone` only allows `'warn' | 'err'`, `warn` is right.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p . && npx vitest run src/components/collab/project/contribution.test.ts src/components/collab/project/OverviewTab.test.tsx src/components/collab/project/attention.test.ts` → green.
- [ ] **Step 5: Commit** `feat(collab-ui): Overview leads with My contribution — four tiles with hours, nights, size and filters`.

---

### Task 4: Project settings card — publishing device, mode, auto-replicate; MetaLine removed

**Files:**
- Create: `src/components/collab/project/publishRunText.ts` and `publishRunText.test.ts` (the run wording shared with Task 5's panel)
- Create: `src/components/collab/project/ProjectSettingsCard.tsx` and `ProjectSettingsCard.test.tsx`
- Delete: `src/components/collab/project/MetaLine.tsx`, `MetaLine.test.tsx`
- Modify: `src/pages/ProjectDetail.tsx` (remove `<MetaLine …/>` :573-579; pass `<ProjectSettingsCard …/>` as `settings` to `OverviewTab`; call `useCollabPublishRun(id)` once and pass it down)
- Modify: `src/pages/ProjectDetail.test.tsx`:
  - replace the meta-line tests (:425-445, :1626, :1638);
  - replace `auto_publish_switch_visible_without_receive` (:804) with a settings-card equivalent.

**Interfaces:**
- Consumes: Task 1 `PublishRunState`; `useCollabLiveState()` (`src/hooks/useCollabLiveState.ts`, already used by ProjectDetail); `deviceLabel` from `usePublishing.ts`; `Seg`, `Card`, `Button`, `Chip`, `ProgressBar`, `StatusDot` from `../../ui`; `Checkbox` from `../../settings/Checkbox`.
- Produces:
  ```ts
  // publishRunText.ts
  export const STAGE_TITLE: Record<PublishStage, string>;
  export function describeLastRun(last: CollabPublishFinished): { text: string; segment: Segment | null; tone: 'ok' | 'warn' | 'error' };
  ```
  ```tsx
  export default function ProjectSettingsCard(props: {
    card: ProjectCard;
    canReceive: boolean;
    run: PublishRunState;
    liveState: string | null;          // from useCollabLiveState()
    onChanged: () => void;             // re-read the card
    onSwitchHere: () => void;          // opens the existing switch confirm
    switchBusy: boolean;
    onOpenMyFrames: () => void;
  }): JSX.Element;
  ```

- [ ] **Step 1: Write the failing tests.**

`publishRunText.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import type { CollabPublishFinished } from '../../../types/models';
import { describeLastRun } from './publishRunText';

const fin = (o: Partial<CollabPublishFinished>): CollabPublishFinished => ({
  projectId: 'p1', publishRunId: 'r', kind: 'publish', trigger: 'manual', outcome: 'done', calibrated: 0,
  announced: 0, updated: 0, stale: 0, heldBack: 0, error: null,
  startedAt: '2026-10-02T10:00:00Z', finishedAt: '2026-10-02T10:03:22Z', ...o,
});

describe('describeLastRun', () => {
  it.each([
    [fin({ kind: 'calibrate', calibrated: 46, heldBack: 2 }), 'Calibrated 46 · 2 held back', 'review', 'ok'],
    [fin({ kind: 'calibrate' }), 'Nothing to calibrate', null, 'ok'],
    [fin({ kind: 'auto', calibrated: 5 }), 'Calibrated 5', 'review', 'ok'],
    [fin({ announced: 3, updated: 1, stale: 2 }), 'Published 4 · 2 back to Ready', 'published', 'ok'],
    [fin({ heldBack: 2 }), 'Nothing new to publish · 2 held back', 'held', 'warn'],
    [fin({ outcome: 'cancelled' }), 'Stopped', null, 'warn'],
    [fin({ outcome: 'refused', error: 'publication of this project is already running' }),
      'Not run — publication of this project is already running', null, 'warn'],
    [fin({ outcome: 'refused', trigger: 'auto', error: 'collab_publishing_device:Obs PC' }),
      'Not run — Obs PC publishes this project', null, 'warn'],
    [fin({ outcome: 'failed', error: 'disk full' }), 'Failed — disk full', null, 'error'],
  ])('%#', (last, text, segment, tone) => {
    expect(describeLastRun(last)).toEqual({ text, segment, tone });
  });
});
```

`ProjectSettingsCard.test.tsx`:

```tsx
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import ProjectSettingsCard from './ProjectSettingsCard';
import type { ProjectCard } from '../../../types/models';
import type { PublishRunState } from './useCollabPublishRun';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn(() => Promise.resolve(() => {})) } }));

const card = (o: Partial<ProjectCard> = {}): ProjectCard => ({
  // copy the full ProjectCard fixture from ProjectDetail.test.tsx `projectCard` and override:
  ...(globalThis as any).__projectCardFixture,
  projectId: 'p1', publishMode: 'manual', publishingHere: true, publishingDevice: { deviceId: 'd1', name: 'Mac Studio' },
  autoReplicate: true, ...o,
});
const idle: PublishRunState = { running: null, last: null, reached: -1, cancel: vi.fn(), cancelBusy: false };
const renderCard = (o: Partial<Parameters<typeof ProjectSettingsCard>[0]> = {}) =>
  render(
    <NotificationProvider>
      <ProjectSettingsCard card={card()} canReceive run={idle} liveState="live" onChanged={vi.fn()}
        onSwitchHere={vi.fn()} switchBusy={false} onOpenMyFrames={vi.fn()} {...o} />
    </NotificationProvider>,
  );

describe('ProjectSettingsCard', () => {
  beforeEach(() => vi.mocked(api.invoke).mockReset());

  it('shows the device, the mode with its help line and auto-replicate', () => {
    renderCard();
    expect(screen.getByText(/This device · Mac Studio/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Manual', pressed: true })).toBeInTheDocument();
    expect(screen.getByText(/Nothing runs on its own/)).toBeInTheDocument();
    expect(screen.getByRole('switch', { name: 'On' })).toBeChecked();
  });

  it('a mode click commits set_project_publish_mode then re-reads the card', async () => {
    const onChanged = vi.fn();
    vi.mocked(api.invoke).mockResolvedValueOnce(undefined);
    renderCard({ onChanged });
    await userEvent.click(screen.getByRole('button', { name: 'Auto-calibrate' }));
    expect(api.invoke).toHaveBeenCalledWith('set_project_publish_mode', { projectId: 'p1', mode: 'autoCalibrate' });
    expect(onChanged).toHaveBeenCalled();
  });

  it('a failed mode write logs, notifies and keeps the stored mode', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('nope'));
    renderCard();
    await userEvent.click(screen.getByRole('button', { name: 'Fully automatic' }));
    expect(err).toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Manual', pressed: true })).toBeInTheDocument();
  });

  it('another device publishing shows Publish from this device', async () => {
    const onSwitchHere = vi.fn();
    renderCard({ card: card({ publishingHere: false, publishingDevice: { deviceId: 'd2', name: 'Observatory' } }), onSwitchHere });
    expect(screen.getByText(/Observatory/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Publish from this device' }));
    expect(onSwitchHere).toHaveBeenCalled();
  });

  it('a running run shows its stage and progress; idle shows the last run line', () => {
    const running = { ...idle, running: { projectId: 'p1', publishRunId: 'r', kind: 'auto', trigger: 'auto', mode: 'autoCalibrate',
      stage: 'calibrating', current: 12, total: 48, currentFile: 'c_x.fits', startedAt: '2026-10-02T10:00:00Z' } } as PublishRunState;
    const { unmount } = renderCard({ run: running });
    expect(screen.getByText(/Calibrating 12 \/ 48/)).toBeInTheDocument();
    unmount();
    renderCard({ run: { ...idle, last: { projectId: 'p1', publishRunId: 'r', kind: 'calibrate', trigger: 'manual', outcome: 'done',
      calibrated: 46, announced: 0, updated: 0, stale: 0, heldBack: 2, error: null, startedAt: '2026-10-02T10:00:00Z',
      finishedAt: '2026-10-02T10:03:22Z' } } });
    expect(screen.getByText(/Calibrated 46/)).toBeInTheDocument();
  });

  it('an auto mode with collaboration off reads Paused', () => {
    renderCard({ card: card({ publishMode: 'automatic' }), liveState: 'off' });
    expect(screen.getByText(/Paused — collaboration is off/)).toBeInTheDocument();
  });

  it('auto-replicate is hidden for a member who cannot receive', () => {
    renderCard({ canReceive: false });
    expect(screen.queryByRole('switch')).toBeNull();
  });
});
```

Replace `(globalThis as any).__projectCardFixture` with a real full `ProjectCard` literal. Copy the one in `ProjectDetail.test.tsx` (`projectCard`, ≈27) into a small shared test fixture module (`src/components/collab/project/testFixtures.ts`) and import it from both files.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/project/publishRunText.test.ts src/components/collab/project/ProjectSettingsCard.test.tsx`.

- [ ] **Step 3: Implement.**

`publishRunText.ts`:

```ts
import type { CollabPublishFinished, PublishStage } from '../../../types/models';
import type { Segment } from './MyFramesTab';
import { publishingDeviceRefusal } from './usePublishing';

/** The one stage wording of a publish run (settings card status, My frames run panel). */
export const STAGE_TITLE: Record<PublishStage, string> = {
  queued: 'Waiting for a compute slot',
  calibrating: 'Calibrating',
  seeding: 'Seeding',
  announcing: 'Announcing',
  versions: 'Posting new versions',
};

/** One line for a finished run, and the My frames segment it points at (spec §7.3, §8.1). */
export function describeLastRun(last: CollabPublishFinished): {
  text: string; segment: Segment | null; tone: 'ok' | 'warn' | 'error';
} {
  const sent = last.announced + last.updated;
  const tail =
    (last.stale > 0 ? ` · ${last.stale} back to Ready` : '') +
    (last.heldBack > 0 ? ` · ${last.heldBack} held back` : '');
  switch (last.outcome) {
    case 'cancelled':
      return { text: 'Stopped', segment: null, tone: 'warn' };
    case 'refused': {
      const device = last.error ? publishingDeviceRefusal(last.error) : null;
      return { text: `Not run — ${device ? `${device} publishes this project` : (last.error ?? 'refused')}`, segment: null, tone: 'warn' };
    }
    case 'failed':
      return { text: `Failed — ${last.error ?? 'unknown error'}`, segment: null, tone: 'error' };
    case 'done':
      if (sent > 0) return { text: `Published ${sent}${tail}`, segment: 'published', tone: 'ok' };
      if (last.calibrated > 0) return { text: `Calibrated ${last.calibrated}${tail}`, segment: 'review', tone: 'ok' };
      if (last.heldBack > 0) {
        return {
          text: `${last.kind === 'calibrate' ? 'Nothing calibrated' : 'Nothing new to publish'}${tail}`,
          segment: 'held', tone: 'warn',
        };
      }
      return { text: last.kind === 'calibrate' ? 'Nothing to calibrate' : 'Nothing to publish', segment: null, tone: 'ok' };
  }
}
```

`ProjectSettingsCard.tsx`:

```tsx
import { useState } from 'react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import { Button, Card, Chip, ProgressBar, Seg, StatusDot } from '../../ui';
import { Checkbox } from '../../settings/Checkbox';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { deviceLabel } from './usePublishing';
import { describeLastRun, STAGE_TITLE } from './publishRunText';
import type { PublishRunState } from './useCollabPublishRun';
import type { ProjectCard, PublishMode } from '../../../types/models';

const MODES: { value: PublishMode; label: string; help: string }[] = [
  { value: 'manual', label: 'Manual', help: 'Nothing runs on its own. You calibrate, review and publish from My frames.' },
  { value: 'autoCalibrate', label: 'Auto-calibrate',
    help: 'New passing frames are calibrated after scans, analysis or new masters, then wait in To review until you publish them. Runs while Athenaeum is open and signed in to the hub.' },
  { value: 'automatic', label: 'Fully automatic',
    help: 'New passing frames are calibrated and published with no review. For a remote rig nobody watches. Runs while Athenaeum is open and signed in to the hub.' },
];

/** Spec 2026-10-01 §7.3 — the project's local settings, each with visible help. */
export default function ProjectSettingsCard({
  card, canReceive, run, liveState, onChanged, onSwitchHere, switchBusy, onOpenMyFrames,
}: {
  card: ProjectCard; canReceive: boolean; run: PublishRunState; liveState: string | null;
  onChanged: () => void; onSwitchHere: () => void; switchBusy: boolean; onOpenMyFrames: () => void;
}) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState<'mode' | 'replicate' | null>(null);

  const write = async (which: 'mode' | 'replicate', cmd: string, args: Record<string, unknown>) => {
    setBusy(which);
    try {
      await api.invoke(cmd, { projectId: card.projectId, ...args });
      onChanged();
    } catch (err) {
      console.error(`[projects] ${cmd} failed:`, err);
      notify({
        title: which === 'mode' ? 'Could not change the publishing mode' : 'Could not change auto-replicate',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project', tone: 'warning', hasErrors: true,
      });
    } finally {
      setBusy(null);
    }
  };

  const mode = MODES.find((m) => m.value === card.publishMode) ?? MODES[0];
  const paused = card.publishMode !== 'manual' && (liveState === 'off' || liveState === 'signedOut');
  const r = run.running;

  return (
    <section aria-label="Project settings">
    <Card title="Project settings" subtitle="this device only">
      <section className="border-t-0 pb-2.5">
        <h3 className="mb-1.5 text-[11px] uppercase tracking-[.04em] text-content-faint">Publishing device</h3>
        <div className="flex flex-wrap items-center gap-2 text-[13px] text-content">
          {card.publishingHere ? (
            <><StatusDot state="live" /> This device · {deviceLabel(card.publishingDevice?.name ?? null)}</>
          ) : card.publishingDevice ? (
            <>
              {deviceLabel(card.publishingDevice.name)}
              <Button size="sm" onClick={onSwitchHere} disabled={switchBusy}>Publish from this device</Button>
            </>
          ) : (
            'Nobody is publishing to this project yet'
          )}
        </div>
        <p className="mt-1 text-[12px] leading-[1.45] text-content-faint">
          One device per account publishes to a project. Frames on your other devices are not published from them.
        </p>
      </section>

      <section className="border-t border-line py-2.5">
        <h3 className="mb-1.5 text-[11px] uppercase tracking-[.04em] text-content-faint">Publishing</h3>
        <Seg
          options={MODES.map((m) => ({ value: m.value, label: m.label }))}
          value={card.publishMode}
          onChange={(v: PublishMode) => { if (v !== card.publishMode && busy === null) void write('mode', 'set_project_publish_mode', { mode: v }); }}
        />
        <p className="mt-1 text-[12px] leading-[1.45] text-content-faint">{mode.help}</p>
        <div className="mt-2 rounded-md border border-line bg-surface px-2.5 py-2 text-[12px] text-content-muted">
          {r ? (
            <>
              <div className="flex items-center gap-2">
                <Chip tone="info">{r.trigger}</Chip>
                <b className="font-semibold text-content">{STAGE_TITLE[r.stage]} {r.current} / {r.total}</b>
                <span className="flex-1" />
                <Button variant="link" size="sm" onClick={onOpenMyFrames}>Open My frames →</Button>
              </div>
              <div className="my-1.5"><ProgressBar percent={r.total > 0 ? (100 * r.current) / r.total : 0} /></div>
              {r.currentFile && <div className="truncate font-mono text-[11.5px]">{r.currentFile}</div>}
            </>
          ) : paused ? (
            'Paused — collaboration is off'
          ) : run.last ? (
            <LastRunLine last={run.last} />
          ) : (
            'No run yet'
          )}
        </div>
      </section>

      {canReceive && (
        <section className="border-t border-line pt-2.5">
          <h3 className="mb-1.5 text-[11px] uppercase tracking-[.04em] text-content-faint">Auto-replicate</h3>
          <Checkbox
            role="switch"
            checked={card.autoReplicate}
            disabled={busy !== null}
            label={card.autoReplicate ? 'On' : 'Off'}
            onChange={(v: boolean) => void write('replicate', 'set_project_auto_replicate', { enabled: v })}
          />
          <p className="mt-1 text-[12px] leading-[1.45] text-content-faint">
            New approved contributions download to this device automatically. Every member who holds a frame helps distribute it.
          </p>
        </section>
      )}
    </Card>
    </section>
  );
}

function LastRunLine({ last }: { last: NonNullable<PublishRunState['last']> }) {
  const d = describeLastRun(last);
  return (
    <span>
      Last run · <b className={d.tone === 'error' ? 'font-semibold text-error' : 'font-semibold text-content'}>{d.text}</b>
      {' · '}{formatTimestamp(last.finishedAt, { seconds: true })} · {last.trigger}
    </span>
  );
}
```

Check `StatusDot`'s real `state` values and `deviceLabel`'s argument against the code.

Don't add new primitives.

`ProjectDetail.tsx`:
- Delete the MetaLine import and render (:573-579). The header keeps one row.
- Call `const run = useCollabPublishRun(id);` near `usePublishing`.
- Pass `settings={<ProjectSettingsCard card={c} canReceive={canReceive} run={run} liveState={liveState} onChanged={() => void loadDetail()} onSwitchHere={() => setSwitchConfirm(true)} switchBusy={publishing.switchBusy} onOpenMyFrames={() => selectTab('mine')} />}` to `OverviewTab`.
- Keep `run` available for Task 5 (pass it to `MyFramesTab`).
- Delete `MetaLine.tsx` and `MetaLine.test.tsx`.
- Port the ProjectDetail meta-line tests to the settings card on the Overview. The A6 "Publish from here" assertion becomes "Publish from this device" in the card, and the switch-confirm flow is unchanged.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p . && npx vitest run src/components/collab/project/publishRunText.test.ts src/components/collab/project/ProjectSettingsCard.test.tsx src/pages/ProjectDetail.test.tsx src/components/collab/project/OverviewTab.test.tsx` → green.
- [ ] **Step 5: Commit** `feat(collab-ui): Project settings card — publishing device, mode with help, auto-replicate; meta line removed`.

---
### Task 5: My frames — To review segment, Calibrate / Don't publish / Release / Update, the run panel

**Files:**
- Create: `src/components/collab/project/PublishRunPanel.tsx` and `PublishRunPanel.test.tsx`
- Create: `src/components/collab/project/useWithhold.tsx` and `useWithhold.test.tsx` (Don't publish / Release, shared with Task 8's Blink)
- Modify: `src/components/collab/project/publishRunText.ts` (add `MODE_LABEL`); `ProjectSettingsCard.tsx` takes its mode labels from it
- Modify: `src/components/ui/SegmentTiles.tsx` (optional `sub`, tone `purple`), `src/components/ui/Chip.tsx` (tone `pur`) and `src/components/ui/primitives.test.tsx`
- Modify: `src/components/collab/project/MyFramesTab.tsx` and `MyFramesTab.test.tsx`
- Modify: `src/components/collab/project/PublishConfirmDialog.tsx` and test (`estimatedBytes` → exact `bytes`)
- Modify: `src/components/collab/LinkObjectDialog.tsx` (F6) and its test, if one exists; otherwise add the case to `MyFramesTab.test.tsx`'s link-dialog tests
- Modify: `src/pages/ProjectDetail.tsx` (pass the run and the new callbacks; exact publish size; drop `APPROX_FRAME_BYTES` :75) and `ProjectDetail.test.tsx`

**Interfaces:**
- Consumes:
  - Task 1 `PublishRunState`, `STEP_ORDER`, `stepIndex`;
  - Task 2 `Segment`, `usePublishing().calibrate` / `calibrateBusy` / `calibrateError`;
  - Task 3 `contributionTiles`;
  - Task 4 `STAGE_TITLE`, `describeLastRun`.
- Produces:
  ```ts
  // publishRunText.ts
  export const MODE_LABEL: Record<PublishMode, string>; // Manual · Auto-calibrate · Fully automatic

  // useWithhold.tsx
  export interface WithholdTarget { frameId: number; prepared: boolean }
  export function useWithhold(projectId: string, onChanged: () => void): {
    dontPublish: (targets: WithholdTarget[]) => Promise<boolean>; // false = cancelled or failed
    release: (frameIds: number[]) => Promise<boolean>;
    busy: boolean;
    dialog: JSX.Element | null; // the caller renders it
  };

  // PublishRunPanel.tsx
  export default function PublishRunPanel(props: { run: PublishRunState; onOpenSegment: (s: Segment) => void }): JSX.Element | null;

  // MyFramesTabProps gains
  run: PublishRunState;
  onCalibrate: (frameIds: number[]) => void;
  calibrateBusy: boolean;
  calibrateError: string | null;
  onUpdate: (frameIds: number[]) => void; // F1: publish_collab_frames with these ids, no confirm

  // PublishConfirmDialog: `estimatedBytes: number` → `bytes: number`
  ```

- [ ] **Step 1: Write the failing tests.**

`PublishRunPanel.test.tsx`:

```tsx
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import PublishRunPanel from './PublishRunPanel';
import type { PublishRunState } from './useCollabPublishRun';
import type { CollabPublishProgress } from '../../../types/models';

const prog = (o: Partial<CollabPublishProgress>): CollabPublishProgress => ({
  projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual', mode: null, stage: 'calibrating',
  current: 12, total: 48, currentFile: 'M42_Ha_0012.fits', startedAt: new Date(Date.now() - 134_000).toISOString(), ...o,
});
const state = (o: Partial<PublishRunState>): PublishRunState => ({
  running: null, last: null, reached: -1, cancel: vi.fn(async () => {}), cancelBusy: false, ...o,
});

describe('PublishRunPanel', () => {
  it('a running calibrate shows its title, why, steps, file, elapsed and Cancel', () => {
    const run = state({ running: prog({}), reached: 1 });
    render(<PublishRunPanel run={run} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('manual')).toBeInTheDocument();
    expect(screen.getByText('Calibrating 12 of 48')).toBeInTheDocument();
    expect(screen.getByText('you clicked Calibrate')).toBeInTheDocument();
    expect(screen.getByText('✓ Queued')).toBeInTheDocument();
    expect(screen.getByText('Calibrate 12 / 48')).toHaveAttribute('aria-current', 'step');
    expect(screen.getByText('M42_Ha_0012.fits')).toBeInTheDocument();
    expect(screen.getByText(/2m 1[34]s elapsed/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(run.cancel).toHaveBeenCalled();
  });

  it('an automatic run reads the mode and "started automatically"', () => {
    render(<PublishRunPanel run={state({ running: prog({ kind: 'auto', trigger: 'auto', mode: 'automatic', stage: 'seeding', current: 31 }), reached: 2 })} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('Fully automatic · seeding 31 of 48')).toBeInTheDocument();
    expect(screen.getByText('started automatically')).toBeInTheDocument();
    expect(screen.getByText('✓ Calibrate')).toBeInTheDocument();
    expect(screen.getByText('Seed 31 / 48')).toHaveAttribute('aria-current', 'step');
  });

  it('F3: a stage that re-enters an earlier step never moves a finished step back', () => {
    render(<PublishRunPanel run={state({ running: prog({ kind: 'auto', trigger: 'auto', mode: 'automatic', stage: 'calibrating', current: 1, total: 2 }), reached: 3 })} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('✓ Seed')).toBeInTheDocument();
    expect(screen.getByText('Announce 1 / 2')).toHaveAttribute('aria-current', 'step');
  });

  it('F9: a publish run regenerating an update lights no step', () => {
    render(<PublishRunPanel run={state({ running: prog({ kind: 'publish', stage: 'calibrating', current: 1, total: 3 }), reached: 1 })} onOpenSegment={vi.fn()} />);
    expect(screen.queryByRole('listitem', { current: 'step' })).toBeNull();
    expect(screen.getByText('Calibrating 1 of 3')).toBeInTheDocument();
  });

  it('a queued run says it waits for the compute slot', () => {
    render(<PublishRunPanel run={state({ running: prog({ stage: 'queued', current: 0 }), reached: 0 })} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('Waiting for a compute slot')).toBeInTheDocument();
    expect(screen.getByText('one compute slot · other runs wait')).toBeInTheDocument();
  });

  it('a finished run shows its line and links the segment it points at', () => {
    const onOpenSegment = vi.fn();
    render(<PublishRunPanel run={state({ last: { projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual',
      outcome: 'done', calibrated: 46, announced: 0, updated: 0, stale: 0, heldBack: 2, error: null,
      startedAt: '2026-10-01T14:00:00Z', finishedAt: '2026-10-01T14:03:22Z' } })} onOpenSegment={onOpenSegment} />);
    expect(screen.getByText('done')).toBeInTheDocument();
    expect(screen.getByText(/Calibrated 46 · 2 held back · .* · manual/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Review →' }));
    expect(onOpenSegment).toHaveBeenCalledWith('review');
  });

  it('renders nothing with no run and no last run', () => {
    const { container } = render(<PublishRunPanel run={state({})} onOpenSegment={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('Cancel is disabled while the cancel is in flight', () => {
    render(<PublishRunPanel run={state({ running: prog({}), reached: 1, cancelBusy: true })} onOpenSegment={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
  });
});
```

`useWithhold.test.tsx` renders a tiny harness component that calls the hook and renders `dialog`, inside `NotificationProvider`, with `api` mocked:

```tsx
it('Don\'t publish with prepared frames confirms, names the files, then writes withheld=true', async () => {
  vi.mocked(api.invoke).mockResolvedValueOnce(2);
  const onChanged = vi.fn();
  const { result } = renderWithhold(onChanged); // helper: renderHook inside NotificationProvider, renders result.current.dialog
  let done!: Promise<boolean>;
  act(() => { done = result.current.dontPublish([{ frameId: 1, prepared: true }, { frameId: 2, prepared: false }]); });
  expect(await screen.findByText(/1 calibrated file will be deleted/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: "Don't publish" }));
  await expect(done).resolves.toBe(true);
  expect(api.invoke).toHaveBeenCalledWith('set_collab_frames_withheld', { projectId: 'p1', frameIds: [1, 2], withheld: true });
  expect(onChanged).toHaveBeenCalled();
});

it('Don\'t publish on Ready frames only acts at once with no dialog', async () => {
  vi.mocked(api.invoke).mockResolvedValueOnce(1);
  const { result } = renderWithhold(vi.fn());
  await act(async () => { await result.current.dontPublish([{ frameId: 5, prepared: false }]); });
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(api.invoke).toHaveBeenCalledWith('set_collab_frames_withheld', { projectId: 'p1', frameIds: [5], withheld: true });
});

it('cancelling the confirm writes nothing and resolves false', async () => {
  const { result } = renderWithhold(vi.fn());
  let done!: Promise<boolean>;
  act(() => { done = result.current.dontPublish([{ frameId: 1, prepared: true }]); });
  fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
  await expect(done).resolves.toBe(false);
  expect(api.invoke).not.toHaveBeenCalled();
});

it('a failed write logs, notifies and resolves false', async () => {
  const err = vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.mocked(api.invoke).mockRejectedValueOnce(new Error('withholding a published frame is refused'));
  const onChanged = vi.fn();
  const { result } = renderWithhold(onChanged);
  await act(async () => { await expect(result.current.release([7])).resolves.toBe(false); });
  expect(err).toHaveBeenCalled();
  expect(onChanged).not.toHaveBeenCalled();
});
```

`MyFramesTab.test.tsx` (add; rewrite tests 2 and "Ready shows Publish all N", which become Calibrate):
- `it('four segment tiles read count, hours and nights')`: rows with ready/review/published/held. The tiles show `Ready to calibrate`, `To review`, `Published`, `Held back`, and the ready tile shows `1h 00m · 1 night`.
- `it('Ready: "Calibrate all 2" calls onCalibrate with both ids; Don\'t publish withholds at once')`.
- `it('dont_publish_on_review_frames_confirms_and_names_the_files')`:
  1. Two `review` rows with `calibratedPath` set; select all; click `Don't publish 2`.
  2. The dialog says `2 calibrated files will be deleted`; confirm.
  3. `set_collab_frames_withheld { projectId, frameIds: [the two ids], withheld: true }` is invoked, then `onReload` is called.
- `it('To review: "Publish all 2" calls onRequestPublish')`.
- `it('Published: Update is offered only for update-pending frames and calls onUpdate with no confirm (F1)')`: one `contributorState: 'updatePending'` row plus one plain row; select all → `Update 1 of 2` → `onUpdate([id])`.
- `it('Held back: Release is offered only for withheld frames')`: a `withheld: true` row plus a threshold row; select all → `Release 1 of 2` → invoke `withheld: false`.
- `it('the run panel sits above the segment tiles while a run is active')`.
- `it('the segment summary line reads frames, hours and size')`: `2 frames · 1h 00m · …`.
- `it('an unlink refused by a running publish says so (F6)')`: the link dialog's Unlink rejects with `publication of this project is already running`. The notification detail reads `A publish run is in progress — try again when it ends.`

`PublishConfirmDialog.test.tsx`: the size line reads `Size 142 MB` (exact). There is no "Estimated" or "≈".

`primitives.test.tsx`: `SegmentTiles` renders a tile's `sub` under its label.

`ProjectDetail.test.tsx`:
- `it('the publish confirm shows the exact Σ calibratedBytes of the chosen frames')`: two review rows with `calibratedBytes` 1 MiB and 2 MiB → `3.0 MB` (match `formatSize`'s output).
- `it('Calibrate in My frames invokes calibrate_collab_frames with the selection')`.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/project/PublishRunPanel.test.tsx src/components/collab/project/useWithhold.test.tsx src/components/collab/project/MyFramesTab.test.tsx src/components/collab/project/PublishConfirmDialog.test.tsx src/components/ui/primitives.test.tsx src/pages/ProjectDetail.test.tsx`.

- [ ] **Step 3: Implement.**

`publishRunText.ts`: add the following, and make `ProjectSettingsCard`'s `MODES` read `label: MODE_LABEL[value]`:

```ts
export const MODE_LABEL: Record<PublishMode, string> = {
  manual: 'Manual',
  autoCalibrate: 'Auto-calibrate',
  automatic: 'Fully automatic',
};
```

`SegmentTiles.tsx`: `tiles` items gain `sub?: ReactNode`, rendered after the label as `<span className="text-[11.5px] text-content-faint">{t.sub}</span>`. `TONE` gains `purple: 'text-purple'`. Existing callers are unchanged.

`PublishRunPanel.tsx`:

```tsx
import { useEffect, useState } from 'react';
import { Button, Chip, ProgressBar } from '../../ui';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { describeLastRun, MODE_LABEL, STAGE_TITLE } from './publishRunText';
import { STEP_ORDER, stepIndex, type PublishRunState } from './useCollabPublishRun';
import type { Segment } from './MyFramesTab';
import type { PublishRunKind, PublishStage } from '../../../types/models';

interface Step { label: string; stages: PublishStage[] }
const QUEUED: Step = { label: 'Queued', stages: ['queued'] };
const CALIBRATE: Step = { label: 'Calibrate', stages: ['calibrating'] };
const SEED: Step = { label: 'Seed', stages: ['seeding'] };
const ANNOUNCE: Step = { label: 'Announce', stages: ['announcing', 'versions'] };
/** Plan F9 — the steps each run kind shows. */
const STEPS: Record<PublishRunKind, Step[]> = {
  calibrate: [QUEUED, CALIBRATE],
  publish: [SEED, ANNOUNCE],
  republish: [QUEUED, CALIBRATE, SEED, ANNOUNCE],
  auto: [QUEUED, CALIBRATE, SEED, ANNOUNCE],
};
const WHY: Record<PublishRunKind, string> = {
  calibrate: 'you clicked Calibrate', publish: 'you clicked Publish', republish: 'you clicked Republish', auto: 'started automatically',
};
const ACTION: Record<Segment, string> = { review: 'Review →', published: 'Open Published →', held: 'Open Held back →', ready: 'Open Ready →' };

function formatElapsed(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`;
}

/** Spec 2026-10-01 §8.1 — the publish run of this project, above the segments. */
export default function PublishRunPanel({ run, onOpenSegment }: { run: PublishRunState; onOpenSegment: (s: Segment) => void }) {
  const r = run.running;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!r) return undefined;
    const t = setInterval(() => setNow(Date.now()), 1000); // a display tick, not a poll
    return () => clearInterval(t);
  }, [r?.publishRunId]); // eslint-disable-line react-hooks/exhaustive-deps

  if (r) {
    const auto = r.trigger === 'auto';
    const queued = r.stage === 'queued';
    const title = queued
      ? STAGE_TITLE.queued
      : auto && r.mode
        ? `${MODE_LABEL[r.mode]} · ${STAGE_TITLE[r.stage].toLowerCase()} ${r.current} of ${r.total}`
        : `${STAGE_TITLE[r.stage]} ${r.current} of ${r.total}`;
    const at = STEP_ORDER[Math.max(0, run.reached)];
    return (
      <section aria-label="Publish run" className="mb-2.5 rounded-md border border-line bg-surface-elevated px-3 py-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <Chip tone={auto ? 'pur' : 'info'}>{auto ? 'auto' : 'manual'}</Chip>
          <b className="text-[13px] font-semibold text-content">{title}</b>
          <span className="text-[12px] text-content-faint">{WHY[r.kind]}</span>
          <span className="flex-1" />
          <Button size="sm" onClick={() => void run.cancel()} disabled={run.cancelBusy}>Cancel</Button>
        </div>
        <ol className="mt-2 flex flex-wrap items-center gap-1.5 text-[12px]">
          {STEPS[r.kind].map((st, i, all) => {
            const active = st.stages.includes(at);
            const done = !active && Math.max(...st.stages.map(stepIndex)) < run.reached;
            return (
              <li key={st.label} aria-current={active ? 'step' : undefined}
                className={active ? 'font-semibold text-accent' : done ? 'text-success' : 'text-content-faint'}>
                {done ? `✓ ${st.label}` : active ? `${st.label} ${r.current} / ${r.total}` : st.label}
                {i < all.length - 1 && <span className="ml-1.5 text-content-faint">→</span>}
              </li>
            );
          })}
        </ol>
        <div className="my-1.5"><ProgressBar percent={r.total > 0 ? (100 * r.current) / r.total : 0} /></div>
        <div className="flex flex-wrap gap-3 text-[11.5px] text-content-muted">
          {r.currentFile && <span className="truncate font-mono">{r.currentFile}</span>}
          <span>{formatElapsed((now - Date.parse(r.startedAt)) / 1000)} elapsed</span>
          {queued && <span>one compute slot · other runs wait</span>}
        </div>
      </section>
    );
  }
  if (!run.last) return null;
  const d = describeLastRun(run.last);
  return (
    <section aria-label="Last publish run" className="mb-2.5 flex flex-wrap items-center gap-2 rounded-md border border-line px-3 py-2 text-[12px] text-content-muted">
      <Chip tone={d.tone === 'ok' ? 'ok' : d.tone === 'warn' ? 'warn' : 'err'}>{run.last.outcome}</Chip>
      <span className={d.tone === 'error' ? 'text-error' : undefined}>
        {d.text} · {formatTimestamp(run.last.finishedAt, { seconds: true })} · {run.last.trigger}
      </span>
      {d.segment && <Button variant="link" size="sm" onClick={() => onOpenSegment(d.segment!)}>{ACTION[d.segment]}</Button>}
    </section>
  );
}
```

`Chip.tsx` gains the tone `pur: 'bg-purple/15 text-purple'` (the mockup's `c-pur`; the `purple` token has no `muted` shade) and nothing else. The F3 test passes `reached: 3` (announcing) while `stage` is `calibrating`. The strip reads `reached`, not `stage`, so the lit step is Announce and Seed is done.

`useWithhold.tsx`:

```tsx
import { useCallback, useRef, useState, type JSX } from 'react';
import { api } from '../../../api';
import ConfirmDialog from '../../ConfirmDialog';
import { useNotifications } from '../../../contexts/NotificationContext';

export interface WithholdTarget { frameId: number; prepared: boolean }

/** Spec 2026-10-01 §4.5 / plan F2 — Don't publish (withheld=true) and Release (withheld=false).
 *  Confirms only when calibrated files are deleted. */
export function useWithhold(projectId: string, onChanged: () => void) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState(false);
  const [ask, setAsk] = useState<{ targets: WithholdTarget[]; prepared: number } | null>(null);
  const resolver = useRef<((ok: boolean) => void) | null>(null);
  const changed = useRef(onChanged);
  changed.current = onChanged;

  const write = useCallback(async (frameIds: number[], withheld: boolean): Promise<boolean> => {
    setBusy(true);
    try {
      await api.invoke<number>('set_collab_frames_withheld', { projectId, frameIds, withheld });
      changed.current();
      return true;
    } catch (err) {
      console.error('[projects] set_collab_frames_withheld failed:', err);
      notify({
        title: withheld ? "Could not withhold the frames" : 'Could not release the frames',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project', tone: 'warning', hasErrors: true,
      });
      return false;
    } finally {
      setBusy(false);
    }
  }, [projectId, notify]);

  const dontPublish = useCallback((targets: WithholdTarget[]): Promise<boolean> => {
    const prepared = targets.filter((t) => t.prepared).length;
    if (prepared === 0) return write(targets.map((t) => t.frameId), true);
    return new Promise<boolean>((resolve) => {
      resolver.current?.(false);
      resolver.current = resolve;
      setAsk({ targets, prepared });
    });
  }, [write]);

  const release = useCallback((frameIds: number[]) => write(frameIds, false), [write]);

  const settle = (ok: boolean) => { const r = resolver.current; resolver.current = null; setAsk(null); r?.(ok); };

  const dialog: JSX.Element | null = ask && (
    <ConfirmDialog
      isOpen
      title={`Don't publish ${ask.targets.length} ${ask.targets.length === 1 ? 'frame' : 'frames'}?`}
      message={`${ask.prepared} calibrated ${ask.prepared === 1 ? 'file will be deleted' : 'files will be deleted'}. The frames move to Held back as "Withheld by you"; Release brings them back to Ready.`}
      confirmText="Don't publish"
      confirmDanger
      onConfirm={() => {
        const r = resolver.current;
        resolver.current = null;
        const ids = ask.targets.map((x) => x.frameId);
        setAsk(null);
        void write(ids, true).then((ok) => r?.(ok));
      }}
      onCancel={() => settle(false)}
    />
  );

  return { dontPublish, release, busy, dialog };
}
```

`MyFramesTab.tsx`:
- `Segment` tiles: four, from `contributionTiles(rows ?? [])` (Task 3):
  - `{ value: 'ready', n, label: 'Ready to calibrate', tone: 'accent', sub }`
  - `review`: `'To review'`, `purple`
  - `published`: `'Published'`, `success`
  - `held`: `'Held back'`, `warning`

  `sub = `${formatDurationPadded(t.seconds)} · ${t.nights} ${t.nights === 1 ? 'night' : 'nights'}``.
- Above the tiles row: `<PublishRunPanel run={run} onOpenSegment={onSegment} />`.
- Between the tiles row and the table: one summary line for the current segment: `${n} frames · ${formatDurationPadded(seconds)} · ${formatSize(bytes)}` (`text-[12px] text-content-faint`).
- `reviewRows = useMemo(() => (rows ?? []).filter((r) => r.segment === 'review').map(fromOwn), [rows])`.
- `const withhold = useWithhold(projectId, onReload);`. Render `{withhold.dialog}` beside the other dialogs.
- `const asTargets = (vs: FrameVM[]) => vs.map((v) => ({ frameId: v.frameId!, prepared: v.own?.calibratedPath != null }));`
- Actions:
  ```ts
  const readyActions: TableAction[] = [
    { id: 'calibrate', verb: 'Calibrate', eligible: () => true, primary: true, busy: calibrateBusy,
      run: (t) => onCalibrate(t.map((v) => v.frameId!)) },
    { id: 'withhold', verb: "Don't publish", eligible: () => true, busy: withhold.busy,
      run: (t) => void withhold.dontPublish(asTargets(t)) },
  ];
  const reviewActions: TableAction[] = [
    { id: 'publish', verb: 'Publish', eligible: () => true, primary: true, busy: publishBusy,
      run: (t) => onRequestPublish(t.map((v) => v.frameId!)) },
    { id: 'withhold', verb: "Don't publish", eligible: () => true, busy: withhold.busy,
      run: (t) => void withhold.dontPublish(asTargets(t)) },
  ];
  // publishedActions gains, first:
  { id: 'update', verb: 'Update', eligible: (v) => v.own?.contributorState === 'updatePending', busy: publishBusy,
    run: (t) => onUpdate(t.map((v) => v.frameId!)) },
  // heldActions gains, first:
  { id: 'release', verb: 'Release', eligible: (v) => v.own?.withheld === true, primary: true, busy: withhold.busy,
    run: (t) => void withhold.release(t.map((v) => v.frameId!)) },
  ```
- The `review` table: `<ProjectFrameTable key={`${projectId}.review`} tableId="review" scope={projectId} rows={reviewRows} actions={reviewActions} onOpen={onOpen} activeKey={activeKey} emptyText="Nothing to review — calibrated frames wait here until you publish them." />`.
- The ready empty text becomes `'Nothing ready — new frames appear here once they pass the gate.'`.
- `calibrateError` renders like `republishError`, on its own line.

`PublishConfirmDialog.tsx`: rename the prop to `bytes`. The line becomes `<p …>Size {formatSize(bytes)}</p>`.

`LinkObjectDialog.tsx` (F6): in the catch, if the message contains `already running`, the detail is `A publish run is in progress — try again when it ends.`; otherwise it keeps the raw message. The `console.error` stays.

`ProjectDetail.tsx`:
- `const publishBytes = useMemo(() => (own ?? []).filter((r) => publishIds?.includes(r.frameId)).reduce((s, r) => s + (r.calibratedBytes ?? r.byteSize), 0), [own, publishIds]);`. Use the page's real own-rows state name.
- Pass `bytes={publishBytes}` and delete `APPROX_FRAME_BYTES`.
- `MyFramesTab` gets:
  - `run={run}` (from Task 4's `useCollabPublishRun(id)`);
  - `onCalibrate={(ids) => void publishing.calibrate(ids)}`;
  - `calibrateBusy={publishing.calibrateBusy}`;
  - `calibrateError={publishing.calibrateError}`;
  - `onUpdate={(ids) => void publishing.publish(ids)}`.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p . && npx vitest run src/components/collab/project/ src/components/ui/primitives.test.tsx src/pages/ProjectDetail.test.tsx` → green.
- [ ] **Step 5: Commit** `feat(collab-ui): My frames — To review segment, Calibrate / Don't publish / Release / Update, publish run panel with Cancel`.

---
### Task 6: Live pill waits for the hub; the page refreshes on changes

**Files:**
- Modify: `src/components/collab/CollabLiveStatus.tsx` (props :114-127, `syncNow` :186-205, the pill :211-238) and `CollabLiveStatus.test.tsx`
- Modify: `src/pages/ProjectDetail.tsx` (pill :566; a `collab-project-synced` listener beside the peers one :309-343; `syncToken`) and `ProjectDetail.test.tsx`
- Modify: `src/components/collab/project/ModerationTab.tsx` (load effect :97-100) and its test
- Modify: `src/components/collab/project/ExchangeTab.tsx` (effects :36-38 and :70-72) and its test

**Interfaces:**
- Consumes: the generated `CollabProjectSynced` (`{ projectId, syncedAt: string | null, ok, error: string | null, changed }`); `ProjectCard.syncedAt`.
- Produces:
  - `CollabLiveStatus` gains `projectId?: string`. With it, the pill click waits for the project's confirmation (spec §6.4). Without it, the old behaviour stays.
  - `export const SYNC_WAIT_MS = 30_000;` (`CollabLiveStatus.tsx`).
  - `ModerationTab` and `ExchangeTab` gain `syncToken?: number`. A change re-reads their own data.

- [ ] **Step 1: Write the failing tests.**

`CollabLiveStatus.test.tsx`: reuse its `api` mock. Capture listeners by event name, unless the file already has a helper:

```ts
const handlers = new Map<string, (p: unknown) => void>();
vi.mocked(api.listen).mockImplementation(async (ev: string, cb: (p: unknown) => void) => {
  handlers.set(ev, cb);
  return () => handlers.delete(ev);
});
const emit = (ev: string, p: unknown) => act(() => handlers.get(ev)?.(p));
```

`get_collab_live_status` resolves `{ state: 'live', … }`, using the file's live fixture. `collab_sync_now` resolves `undefined`. The tests:

```tsx
it('the click waits for this project\'s synced report, then calls onSynced', async () => {
  const onSynced = vi.fn();
  renderPill({ projectId: 'p1', syncedAt: new Date(Date.now() - 50_000).toISOString(), onSynced });
  fireEvent.click(await screen.findByRole('button', { name: /synced/ }));
  expect(await screen.findByText('Syncing…')).toBeInTheDocument();
  emit('collab-project-synced', { projectId: 'p2', syncedAt: new Date(Date.now() + 5).toISOString(), ok: true, error: null, changed: true });
  expect(screen.getByText('Syncing…')).toBeInTheDocument();
  emit('collab-project-synced', { projectId: 'p1', syncedAt: new Date(Date.now() + 5).toISOString(), ok: true, error: null, changed: false });
  await waitFor(() => expect(screen.queryByText('Syncing…')).toBeNull());
  expect(screen.getByRole('button', { name: /synced [0-2] s ago/ })).toBeInTheDocument();
  expect(onSynced).toHaveBeenCalledTimes(1);
});

it('a_not_ok_report_stops_the_pill_and_notifies', async () => {
  const err = vi.spyOn(console, 'error').mockImplementation(() => {});
  const onSynced = vi.fn();
  renderPill({ projectId: 'p1', syncedAt: null, onSynced });
  fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
  emit('collab-project-synced', { projectId: 'p1', syncedAt: null, ok: false, error: 'the hub refused the project (403)', changed: false });
  await waitFor(() => expect(screen.queryByText('Syncing…')).toBeNull());
  expect(await screen.findByText('Sync did not complete')).toBeInTheDocument();
  expect(screen.getByText(/the hub refused the project \(403\)/)).toBeInTheDocument();
  expect(err).toHaveBeenCalled();
  expect(onSynced).not.toHaveBeenCalled();
});

it('an ok report stamped before the click does not end the wait', async () => {
  renderPill({ projectId: 'p1', syncedAt: null });
  fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
  emit('collab-project-synced', { projectId: 'p1', syncedAt: new Date(Date.now() - 5_000).toISOString(), ok: true, error: null, changed: true });
  expect(screen.getByText('Syncing…')).toBeInTheDocument();
});

it('thirty seconds without a report stop the pill and say there was no answer', async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.spyOn(console, 'error').mockImplementation(() => {});
  renderPill({ projectId: 'p1', syncedAt: null });
  fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
  await act(async () => { vi.advanceTimersByTime(SYNC_WAIT_MS); });
  expect(screen.queryByText('Syncing…')).toBeNull();
  expect(screen.getByText(/no answer from the hub/)).toBeInTheDocument();
  vi.useRealTimers();
});

it('F5: a synced report restarts the age without a card re-read', async () => {
  renderPill({ projectId: 'p1', syncedAt: new Date(Date.now() - 50_000).toISOString() });
  expect(await screen.findByRole('button', { name: /synced 5\d s ago/ })).toBeInTheDocument();
  emit('collab-project-synced', { projectId: 'p1', syncedAt: new Date().toISOString(), ok: true, error: null, changed: false });
  expect(await screen.findByRole('button', { name: /synced [0-2] s ago/ })).toBeInTheDocument();
});

it('without projectId the pill keeps calling onSynced right after collab_sync_now', async () => {
  const onSynced = vi.fn();
  renderPill({ syncedAt: null, onSynced });
  fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
  await waitFor(() => expect(onSynced).toHaveBeenCalledTimes(1));
});

it('a failed collab_sync_now ends the wait and notifies Sync now failed', async () => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.mocked(api.invoke).mockImplementation(async (cmd: string) => {
    if (cmd === 'collab_sync_now') throw new Error('not signed in');
    return liveStatus; // the file's live fixture
  });
  renderPill({ projectId: 'p1', syncedAt: null });
  fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
  expect(await screen.findByText('Sync now failed')).toBeInTheDocument();
  expect(screen.queryByText('Syncing…')).toBeNull();
});
```

`renderPill(props)` renders `<NotificationProvider><CollabLiveStatus variant="pill" {...props} /><ToastStack /></NotificationProvider>`, using the providers the file already uses (`useDeviceReplace` may need its provider or mock). Match the regexes to `pillLabel`'s real wording: read `pillLabel` and `formatAge` first, and keep the intent (0–2 s after a report, ~50 s before).

`ProjectDetail.test.tsx`, with fake timers like the existing `collab-peers-changed` throttle tests:
- `it('a changed synced report reloads detail, library and members after 1 s and own frames after 5 s')`.
- `it('an unchanged or foreign-project report reloads nothing')`.
- `it('the pill confirmation re-reads detail, own frames, library and members')`. Click the pill and emit the ok report, then count the four list/detail invokes.
- `it('the pill reads the card syncedAt, not fetchedAt')`.

`ModerationTab.test.tsx`: `it('a new syncToken re-reads the queue')` (rerender with `syncToken` 1 → `list_collab_moderation` twice).

`ExchangeTab.test.tsx`: `it('a new syncToken re-reads the sessions and the project summary')`.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/CollabLiveStatus.test.tsx src/pages/ProjectDetail.test.tsx src/components/collab/project/ModerationTab.test.tsx src/components/collab/project/ExchangeTab.test.tsx`.

- [ ] **Step 3: Implement.**

`CollabLiveStatus.tsx`:

```tsx
export const SYNC_WAIT_MS = 30_000;

/** The newer of two RFC 3339 stamps (plan F5); an unparsable one loses. */
function newer(a: string | null, b: string | null): string | null {
  if (!a) return b;
  if (!b) return a;
  return parseSyncedAt(b) > parseSyncedAt(a) ? b : a;
}
```

Inside the component (new prop `projectId?: string`):

```tsx
const [heard, setHeard] = useState<string | null>(null);
const wait = useRef<{ since: number; timer: ReturnType<typeof setTimeout> } | null>(null);
const onSyncedRef = useRef(onSynced);
onSyncedRef.current = onSynced;

const endWait = () => {
  if (wait.current) clearTimeout(wait.current.timer);
  wait.current = null;
  setSyncing(false);
};

// Spec §6.4: the click's confirmation is this project's next report.
useEffect(() => {
  if (!projectId) return undefined;
  let cancelled = false;
  let unlisten: (() => void) | undefined;
  api
    .listen<CollabProjectSynced>('collab-project-synced', (p) => {
      if (cancelled || p.projectId !== projectId) return;
      if (p.ok && p.syncedAt) setHeard((h) => newer(h, p.syncedAt));
      const w = wait.current;
      if (!w) return;
      if (!p.ok) {
        endWait();
        console.error('[collab] sync report not ok:', p.error);
        notify({ title: 'Sync did not complete', detail: p.error ?? 'the hub did not confirm the project',
          kind: 'project', tone: 'warning', hasErrors: true });
      } else if (p.syncedAt && parseSyncedAt(p.syncedAt) >= w.since) {
        endWait();
        onSyncedRef.current?.();
      }
    })
    .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
    .catch((err) => console.error('[collab] project-synced listen failed:', err));
  return () => {
    cancelled = true;
    unlisten?.();
    if (wait.current) clearTimeout(wait.current.timer);
    wait.current = null;
  };
}, [projectId]); // eslint-disable-line react-hooks/exhaustive-deps

const syncNow = async () => {
  setSyncing(true);
  if (projectId) {
    if (wait.current) clearTimeout(wait.current.timer);
    wait.current = {
      since: Date.now(),
      timer: setTimeout(() => {
        endWait();
        console.error('[collab] sync confirmation timed out', { projectId });
        notify({ title: 'Sync did not complete', detail: 'no answer from the hub',
          kind: 'project', tone: 'warning', hasErrors: true });
      }, SYNC_WAIT_MS),
    };
  }
  try {
    await api.invoke('collab_sync_now');
    if (!projectId) {
      onSynced?.();
      setSyncing(false);
    }
  } catch (err) {
    console.error('[collab] collab_sync_now failed:', err);
    notify({ title: 'Sync now failed', detail: err instanceof Error ? err.message : String(err),
      kind: 'project', tone: 'warning', hasErrors: true });
    endWait();
  }
};
```

- Remove the old `finally { setSyncing(false) }`. With `projectId`, `syncing` ends only through `endWait`.
- The pill label becomes `{syncing && live ? 'Syncing…' : pillLabel(status, elapsed, newer(syncedAt, heard), now)}`. While a reconnect shows `reconnecting`, the pill shows that status and keeps waiting (spec §6.4: the status shows at once).
- The pill stays `disabled={syncing || off}`.
- Update the `syncedAt` prop doc: "The project card's `syncedAt` — the last hub confirmation".

`ProjectDetail.tsx`:
- Pill: `<CollabLiveStatus variant="pill" projectId={id} syncedAt={c.syncedAt} onSynced={reloadAll} />`, where:
  ```ts
  const [syncToken, setSyncToken] = useState(0);
  const reloadAll = useCallback(() => {
    void loadDetail(); void loadOwn(); void loadLibrary(); void loadMembers();
    setSyncToken((n) => n + 1);
  }, [loadDetail, loadOwn, loadLibrary, loadMembers]);
  ```
- Add `const loadDetailRef = useRef(loadDetail); loadDetailRef.current = loadDetail;` beside the other loader refs.
- A second listener effect, separate from the peers one, with its own timers:
  ```ts
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let pageTimer: ReturnType<typeof setTimeout> | undefined;
    let ownTimer: ReturnType<typeof setTimeout> | undefined;
    api
      .listen<CollabProjectSynced>('collab-project-synced', (p) => {
        if (cancelled || p.projectId !== id || !p.changed) return;
        if (pageTimer === undefined) {
          pageTimer = setTimeout(() => {
            pageTimer = undefined;
            void loadDetailRef.current();
            void loadLibraryRef.current();
            void loadMembersRef.current();
            setSyncToken((n) => n + 1);
          }, PEERS_RELOAD_MS);
        }
        if (ownTimer === undefined) {
          ownTimer = setTimeout(() => {
            ownTimer = undefined;
            void loadOwnRef.current();
          }, OWN_RELOAD_MS);
        }
      })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[projects] collab-project-synced listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
      if (pageTimer !== undefined) clearTimeout(pageTimer);
      if (ownTimer !== undefined) clearTimeout(ownTimer);
    };
  }, [id]);
  ```
- `<ExchangeTab … syncToken={syncToken} />` and `<ModerationTab … syncToken={syncToken} />`.

`ModerationTab.tsx`: prop `syncToken?: number`; the load effect's deps become `[load, requireApproval, syncToken]`.

`ExchangeTab.tsx`: prop `syncToken?: number`. The `refreshProject` effect deps become `[projectId, refreshProject, syncToken]`, and the sessions effect deps `[projectId, syncToken]`.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p . && npx vitest run src/components/collab/CollabLiveStatus.test.tsx src/pages/ProjectDetail.test.tsx src/components/collab/project/ModerationTab.test.tsx src/components/collab/project/ExchangeTab.test.tsx` → green.
- [ ] **Step 5: Commit** `feat(collab-ui): Live pill waits for the hub's confirmation; the page refreshes on synced changes`.

---
### Task 7: `BlinkViewer` project mode — project images, actions instead of the Black Hole, overlay-safe keys

**Files:**
- Modify: `src/components/blink/types.ts` (`BlinkFrame`, `BlinkAction`, props)
- Modify: `src/components/BlinkViewer.tsx`:
  - `fitsFrames` :175-180
  - preview load :196-244
  - full-res load :517-560
  - keydown :622-698
  - Black Hole check :700-713
  - canvas size :444-470
  - toolbar :1011-1041
  - `FrameList` :1134-1149
- Modify: `src/components/blink/ToolBar.tsx` (selection block :175-203; new optional `projectActions`)
- Modify: `src/components/blink/FrameList.tsx` (key :195; Locate :227-238; badge)
- Create: `src/components/BlinkViewer.test.tsx` (the first Blink tests)

**Interfaces:**
- Consumes: the generated `CollabFrameRef`, `BlinkSource`; `pushOverlay` / `popOverlay` / `isTopOverlay` from `src/components/ui/overlayStack.ts`.
- Produces (in `src/components/blink/types.ts`):
  ```ts
  export type BlinkFrame = FileWithFrame & {
    key?: string;
    /** Set for calibrated and replica entries: both image loads go through get_collab_frame_image. */
    imageRef?: { projectId: string; frame: CollabFrameRef };
    source?: BlinkSource;
    badge?: string;
  };
  export interface BlinkAction {
    id: string;
    label: (n: number) => string;
    eligible: (f: BlinkFrame) => boolean;
    tone: 'default' | 'warn' | 'danger';
    run: (frames: BlinkFrame[]) => Promise<void> | void;
  }
  // BlinkViewerProps: frames: BlinkFrame[]; plus
  actions?: BlinkAction[];
  contextLabel?: (f: BlinkFrame) => string;
  viewOnly?: boolean;
  // ToolBarProps gains
  projectActions?: { id: string; label: string; tone: 'default' | 'warn' | 'danger'; busy: boolean; onClick: () => void }[];
  // FrameListProps: frames: BlinkFrame[]; plus hideLocate?: boolean
  ```
  The spec writes `imageRef.ref`. The field is `imageRef.frame`, matching the wire key (W8).

**Project mode** means `actions !== undefined`. Every existing caller passes no `actions` and behaves exactly as today.

- [ ] **Step 1: Write the failing tests** (`src/components/BlinkViewer.test.tsx`):

```tsx
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { useState } from 'react';
import { api } from '../api';
import BlinkViewer from './BlinkViewer';
import { DialogShell } from './ui';
import type { BlinkAction, BlinkFrame } from './blink/types';

vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn(() => Promise.resolve(() => {})) } }));
vi.mock('../utils/platform', () => ({ isTauri: true }));

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => 'blob:x');
  URL.revokeObjectURL = vi.fn();
  // jsdom has no 2D context: a context whose every method is a no-op.
  HTMLCanvasElement.prototype.getContext = vi.fn(
    () => new Proxy({}, { get: (_t, k) => (k === 'canvas' ? document.createElement('canvas') : () => {}), set: () => true }),
  ) as never;
});

function entry(i: number, o: Partial<BlinkFrame> = {}): BlinkFrame {
  return {
    file: { id: null, path: `/c/c_${i}.fits`, filename: `c_${i}.fits`, format: 'FITS' } as never,
    frame: { id: 100 + i } as never,
    key: `f${i}`, source: 'calibrated',
    imageRef: { projectId: 'p1', frame: { frameId: i, frameUuid: null } },
    ...o,
  };
}

const invoked = (cmd: string) => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === cmd);

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(async (cmd: string) => {
    if (cmd === 'get_blink_threads_max') return 2;
    if (cmd === 'get_setting') return '';
    if (cmd === 'get_collab_frame_image' || cmd === 'read_fits_image_rustafits') return new Uint8Array([0xff, 0xd8]);
    if (cmd === 'get_blackholed_file_ids') return [];
    return null;
  });
});
afterEach(() => vi.restoreAllMocks());

const withhold = (run = vi.fn()): BlinkAction => ({
  id: 'withhold', label: (n) => `Don't publish (${n})`, eligible: (f) => f.badge !== 'withheld', tone: 'warn', run,
});
const renderBlink = (p: Partial<React.ComponentProps<typeof BlinkViewer>> = {}) =>
  render(<MemoryRouter><BlinkViewer frames={[entry(1), entry(2)]} onClose={vi.fn()} {...p} /></MemoryRouter>);

describe('BlinkViewer — project mode', () => {
  it('with actions: no Black Hole call, no Blackhole/Restore/Locate, the action shows its eligible count', async () => {
    renderBlink({ actions: [withhold()] });
    await waitFor(() => expect(invoked('get_collab_frame_image').length).toBeGreaterThan(0));
    expect(invoked('get_blackholed_file_ids')).toHaveLength(0);
    fireEvent.keyDown(window, { key: 's' });
    expect(await screen.findByRole('button', { name: "Don't publish (1)" })).toBeInTheDocument();
    expect(screen.queryByText(/Blackhole/)).toBeNull();
    expect(screen.queryByTitle('Locate in file browser')).toBeNull();
  });

  it('both image loads use get_collab_frame_image with the frame ref', async () => {
    renderBlink({ actions: [] });
    await waitFor(() =>
      expect(invoked('get_collab_frame_image')[0][1]).toEqual({ projectId: 'p1', frame: { frameId: 1, frameUuid: null } }),
    );
    fireEvent.click(screen.getByTitle(/Switch to full resolution/));
    await waitFor(() =>
      expect(invoked('get_collab_frame_image').some(([, a]) => (a as { resolution?: string }).resolution === 'full')).toBe(true),
    );
    expect(invoked('read_fits_image_rustafits')).toHaveLength(0);
  });

  it('a raw entry without imageRef keeps today\'s load path', async () => {
    renderBlink({ actions: [], frames: [entry(1, { imageRef: undefined, source: 'raw' })] });
    await waitFor(() => expect(invoked('read_fits_image_rustafits').length).toBeGreaterThan(0));
    expect(invoked('get_collab_frame_image')).toHaveLength(0);
  });

  it('an action with no eligible entry in the selection is hidden; run gets the eligible entries only', async () => {
    const run = vi.fn();
    renderBlink({ actions: [withhold(run)], frames: [entry(1, { badge: 'withheld' }), entry(2)] });
    fireEvent.keyDown(window, { key: 's' }); // selects entry 1 (withheld)
    expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull();
    fireEvent.keyDown(window, { key: 'a', ctrlKey: true });
    fireEvent.click(await screen.findByRole('button', { name: "Don't publish (1)" }));
    await waitFor(() => expect(run).toHaveBeenCalledWith([expect.objectContaining({ key: 'f2' })]));
  });

  it('a fresh frames array with new badges keeps the position, the selection and the loaded images', async () => {
    const { rerender } = renderBlink({ actions: [withhold()] });
    await waitFor(() => expect(invoked('get_collab_frame_image')).toHaveLength(2)); // both cached before measuring
    fireEvent.keyDown(window, { key: 'ArrowDown' });
    fireEvent.keyDown(window, { key: 's' });
    const loads = invoked('get_collab_frame_image').length;
    rerender(<MemoryRouter><BlinkViewer frames={[entry(1), entry(2, { badge: 'withheld' })]} onClose={vi.fn()} actions={[withhold()]} /></MemoryRouter>);
    expect(await screen.findAllByText('withheld')).not.toHaveLength(0);
    expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull(); // the selected entry is now withheld
    expect(invoked('get_collab_frame_image').length).toBe(loads);
  });

  it('view only shows the chip and the context label', async () => {
    renderBlink({ actions: [], viewOnly: true, contextLabel: () => 'received from Anna · 2026-09-30 21:14:03' });
    expect(await screen.findByText('View only')).toBeInTheDocument();
    expect(screen.getByText(/received from Anna/)).toBeInTheDocument();
    expect(screen.getAllByText('calibrated').length).toBeGreaterThan(0);
  });

  it('two entries with no file id get distinct list keys', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    renderBlink({ actions: [], frames: [entry(1, { key: 'a' }), entry(2, { key: 'b' })] });
    await screen.findAllByText(/c_\d\.fits/);
    expect(err.mock.calls.some((c) => String(c[0]).includes('same key'))).toBe(false);
  });

  it('keys_typed_in_a_dialog_over_blink_do_not_drive_blink', async () => {
    const onCloseBlink = vi.fn();
    const onCloseDialog = vi.fn();
    function Harness() {
      const [open, setOpen] = useState(true);
      return (
        <MemoryRouter>
          <BlinkViewer frames={[entry(1), entry(2)]} onClose={onCloseBlink} actions={[withhold()]} />
          {open && (
            <DialogShell title="Exclude" onClose={() => { onCloseDialog(); setOpen(false); }}>
              <textarea aria-label="Reason" />
            </DialogShell>
          )}
        </MemoryRouter>
      );
    }
    render(<Harness />);
    const reason = await screen.findByLabelText('Reason');
    reason.focus();
    for (const key of ['s', ' ', 'a', 'ArrowDown', '+']) fireEvent.keyDown(reason, { key });
    expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull(); // 's' selected nothing
    fireEvent.keyDown(reason, { key: 'Escape' });
    expect(onCloseDialog).toHaveBeenCalledTimes(1);
    expect(onCloseBlink).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: 'Escape' }); // the dialog is gone: Escape is Blink's again
    expect(onCloseBlink).toHaveBeenCalledTimes(1);
  });
});

describe('BlinkViewer — existing callers', () => {
  it('without actions it still checks the Black Hole and offers Blackhole on a selection', async () => {
    renderBlink({ frames: [entry(1, { imageRef: undefined, key: undefined, file: { id: 7, path: '/r/a.fits', filename: 'a.fits', format: 'FITS' } as never })] });
    await waitFor(() => expect(invoked('get_blackholed_file_ids')).toHaveLength(1));
    fireEvent.keyDown(window, { key: 's' });
    expect(await screen.findByText(/Blackhole \(1\)/)).toBeInTheDocument();
  });
});
```

Adapt the fixture casts (`as never`) to the real `File` / `Frame` types if the compiler allows a narrower cast; keep the field values. Use the `DialogShell` props as the component defines them. If `getContext`'s Proxy trips a call the renderer makes (e.g. `measureText().width`), return `{ width: 0 }` for that key and note it.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/BlinkViewer.test.tsx`.

- [ ] **Step 3: Implement.**

`BlinkViewer.tsx`:
- Destructure the new props `actions`, `contextLabel`, `viewOnly`. `const projectMode = actions !== undefined;`
- **Snapshot (spec §9.2), project mode only.**
  - `const snapshot = useRef(frames);`
  - `const base = projectMode ? snapshot.current : frames;`
  - `fitsFrames` memoizes over `base`, so its identity, the index caches and `frameIds` stay stable while project rows reload.
  - Badges come from the live prop: `const liveByKey = useMemo(() => new Map(frames.filter((f) => f.key).map((f) => [f.key!, f])), [frames]);` and `const view = (f: BlinkFrame): BlinkFrame => (f.key && liveByKey.get(f.key)) || f;`.
  - Render, eligibility and `run` arguments use `view(fitsFrames[i])`. Loading never does.
- **Image loads.** Add `const frameImage = (f: BlinkFrame, full: boolean) => api.invoke<Uint8Array<ArrayBuffer> | ArrayBuffer | number[]>('get_collab_frame_image', { projectId: f.imageRef!.projectId, frame: f.imageRef!.frame, ...(full ? { resolution: 'full' } : {}) });`.
  - The preview load: `const imageData = frame.imageRef ? await frameImage(frame, false) : <today's isTauri branch>;`.
  - The full-res load: the same, with `true`.
- **Black Hole.** The `get_blackholed_file_ids` effect returns early when `projectMode`. The blackhole confirm modal (:1174-1210) renders only when `!projectMode` (it can never open then; the guard documents it).
- **Actions.**
  ```tsx
  const [actionBusy, setActionBusy] = useState<string | null>(null);
  const selectedViews = useMemo(
    () => [...selectedFrames].map((i) => fitsFrames[i]).filter(Boolean).map(view),
    [selectedFrames, fitsFrames, liveByKey], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const projectActions = actions?.flatMap((a) => {
    const eligible = selectedViews.filter(a.eligible);
    if (eligible.length === 0) return [];
    return [{
      id: a.id, label: a.label(eligible.length), tone: a.tone, busy: actionBusy === a.id,
      onClick: () => {
        setActionBusy(a.id);
        Promise.resolve(a.run(eligible))
          .catch((err) => console.error(`[blink] action ${a.id} failed:`, err))
          .finally(() => setActionBusy(null));
      },
    }];
  });
  ```
  Pass `projectActions={projectMode ? projectActions : undefined}` to `ToolBar`.
- **Context strip.** Under `ToolBar`, inside the same measured header wrapper, when `projectMode`:
  ```tsx
  <div className="flex items-center gap-2 border-b border-line bg-surface px-3 py-1 text-[12px] text-content-muted">
    {cur?.source && <Chip tone={cur.source === 'raw' ? 'mute' : cur.source === 'calibrated' ? 'pur' : 'info'}>{cur.source}</Chip>}
    {cur?.badge && <Chip tone="warn">{cur.badge}</Chip>}
    <span className="truncate font-mono text-content">{cur?.file.filename}</span>
    {cur && contextLabel && <span className="truncate">{contextLabel(cur)}</span>}
    <span className="flex-1" />
    {viewOnly && <Chip tone="mute">View only</Chip>}
  </div>
  ```
  where `const cur = currentFrame ? view(currentFrame) : undefined;`.
- **Overlay stack and keys (spec §9.2, R35).**
  ```ts
  const overlayId = useRef<number | null>(null);
  useLayoutEffect(() => {
    const id = pushOverlay('dialog');
    overlayId.current = id;
    return () => { popOverlay(id); overlayId.current = null; };
  }, []);
  ```
  At the top of `handleKeyPress`:
  ```ts
  const t = e.target as HTMLElement | null;
  if (e.defaultPrevented) return;
  if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
  if (overlayId.current !== null && !isTopOverlay(overlayId.current)) return;
  ```
  This applies to every caller. A dialog opened above Blink owns the keys, which is correct everywhere.
- **F7.** Wrap `ToolBar` and the context strip in `<div ref={headerRef}>`. Then:
  ```ts
  const headerRef = useRef<HTMLDivElement>(null);
  const [headerH, setHeaderH] = useState(48);
  useEffect(() => {
    const el = headerRef.current;
    if (!el || typeof ResizeObserver === 'undefined') return undefined;
    const ro = new ResizeObserver(() => setHeaderH(el.getBoundingClientRect().height || 48));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  ```
  The canvas size uses `window.innerHeight - headerH`, and `headerH` joins that effect's deps.
- `FrameList` gets `hideLocate={projectMode}` and `frames={fitsFrames.map(view)}`, memoized on `[fitsFrames, liveByKey]`.

`ToolBar.tsx`: when `projectActions` is given, the selection block renders those buttons, in place of Restore / Blackhole:
- `warn` → `bg-warning text-surface`;
- `danger` → `bg-error text-white`;
- `default` → `bg-surface-elevated text-content`.

Each button shows a spinner while `busy` and is disabled while any is busy. Hide the selection block's buttons, but not the count, when the list is empty.

`FrameList.tsx`:
- The row key becomes `frame.key ?? frame.file.id ?? index`.
- The Locate button renders only when `!hideLocate`.
- A row with `frame.badge` shows it as a small `text-warning` label before the loading spinner.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p . && npx vitest run src/components/BlinkViewer.test.tsx` → green. Then run every test of an existing Blink caller (`npx vitest run src/pages/FrameSetDetail src/components/dualpane src/components/CalibrationSetTable src/components/missing-metadata`) → unchanged.
- [ ] **Step 5: Commit** `feat(blink): project mode — project images, caller actions instead of the Black Hole, context strip, overlay-safe keys`.

---
### Task 8: Blink from My frames and Library — eligibility, role actions, Exclude over Blink

**Files:**
- Create: `src/components/collab/project/blinkEligibility.ts` and `blinkEligibility.test.ts`
- Create: `src/components/collab/project/ProjectBlink.tsx` and `ProjectBlink.test.tsx`
- Modify: `src/components/collab/project/MyFramesTab.tsx` (a Blink action on all four tables; render `ProjectBlink`) and `MyFramesTab.test.tsx`
- Modify: `src/components/collab/project/LibraryTab.tsx` (a Blink action; render `ProjectBlink`) and `LibraryTab.test.tsx`

**Interfaces:**
- Consumes:
  - Task 5's `useWithhold`;
  - Task 7's `BlinkFrame`, `BlinkAction` and `BlinkViewer` project props;
  - `ExcludeDialog { projectId, frames: FrameVM[], onClose, onDone }`;
  - `FrameVM`, `fromOwn`, `ownFrameKey`;
  - the generated `CollabBlinkEntry`, `CollabFrameRef`.
- Produces:
  ```ts
  // blinkEligibility.ts
  export type BlinkTable = 'ready' | 'review' | 'published' | 'held' | 'library';
  export function blinkRef(v: FrameVM, table: BlinkTable): CollabFrameRef | null; // null = not on this device
  export function blinkBadge(v: FrameVM): string | undefined;
  export function refKey(r: CollabFrameRef): string; // core's entry key: frameUuid ?? `f${frameId}`

  // ProjectBlink.tsx
  export default function ProjectBlink(props: {
    projectId: string;
    table: BlinkTable;
    vms: FrameVM[];                                  // the rows the Blink action ran on
    lookup: (vmKey: string) => FrameVM | undefined;  // the tab's CURRENT rows, every segment
    canModerate: boolean;
    onClose: () => void;
    onChanged: () => void;                           // the tab's reload
  }): JSX.Element;
  ```

- [ ] **Step 1: Write the failing tests.**

`blinkEligibility.test.ts`, built from `fromOwn` / `fromLibrary` fixtures (reuse the `own(...)` / library row helpers of `frames.test.tsx`):

| Table | Row | `blinkRef` |
| ---- | ---- | ---- |
| ready | `path: '/r/a.fits'` | `{ frameId: 1, frameUuid: null }` |
| ready | `path: null` | `null` |
| held | failure kind `blackHole` | `null` |
| review | `calibratedPath: '/c/c_a.fits'` | `{ frameId: 1, frameUuid: null }` |
| review | attested: `calibratedPath: null`, `path` set | `{ frameId: 1, frameUuid: null }` (core serves the original as raw) |
| published | `localState: 'own_held'` | `{ frameId: 1, frameUuid: null }` |
| published | `localState: 'own_missing'` | `null` |
| published | `localState: 'own_changed'` | a ref; `blinkBadge` → `'changed on disk'` |
| library | `localState: 'held'` | `{ frameId: null, frameUuid: 'u1' }` |
| library | `localState: 'wanted'` | `null` |
| library | `localState: 'own_held'` | `{ frameId: null, frameUuid: 'u1' }` |

Plus:
- `blinkBadge` gives `'withheld'` for a withheld own row and `'excluded'` for `accepted === false`.
- `refKey({ frameId: 7, frameUuid: null })` is `'f7'`, and `refKey({ frameId: null, frameUuid: 'u1' })` is `'u1'`.

`ProjectBlink.test.tsx`. Mock `BlinkViewer` to capture its props:

```tsx
let blink: BlinkViewerProps | null = null;
vi.mock('../../BlinkViewer', () => ({
  default: (p: BlinkViewerProps) => { blink = p; return <div data-testid="blink" />; },
}));
```

Wrap renders in `NotificationProvider` + `ToastStack`.

- `it('resolves the refs once and maps entries: imageRef only for calibrated and replica')`:
  - Pass a review VM (frameId 1) and a ready VM (frameId 2), table `review`.
  - `get_collab_blink_frames` is called once with `{ projectId: 'p1', refs: [{ frameId: 1, frameUuid: null }, { frameId: 2, frameUuid: null }] }`. It answers `[{ key: 'f1', source: 'calibrated', … }, { key: 'f2', source: 'raw', … }]`.
  - `blink.frames.map((f) => f.key)` is `['f1', 'f2']`.
  - `frames[0].imageRef` is `{ projectId: 'p1', frame: { frameId: 1, frameUuid: null } }`, and `frames[1].imageRef` is `undefined`.
- `it('own To review: Don\'t publish confirms, writes withheld, and the reload turns the badge to withheld under the same key')`:
  - `await act(() => blink!.actions!.find((a) => a.id === 'withhold')!.run([blink!.frames[0]]))`. Do not await the promise before confirming: start it, click the dialog's `Don't publish`, then await.
  - `set_collab_frames_withheld { withheld: true, frameIds: [1] }` is invoked, and `onChanged` is called.
  - Rerender with a `lookup` whose VM has `own.withheld = true`: `blink.frames[0]` has `badge: 'withheld'` and `key: 'f1'`.
- `it('a member in Library is view only, with no actions and a moderator hint')`:
  - Table `library`, `canModerate: false`: `blink.viewOnly` is `true`, and `blink.actions` is `[]`.
  - `blink.contextLabel(frames[0])` contains `received from Anna` and `Only a moderator can exclude`.
- `it('a moderator in Library gets Exclude from project; it opens ExcludeDialog over Blink with the matching rows')`:
  - Run the exclude action with one frame → the `ExcludeDialog` title is shown, and its `frames` are that VM.
  - Confirm with a reason → `exclude_collab_frame` is invoked, and `onChanged` is called.
- `it('a failed resolve logs, notifies "Could not open Blink" and closes')`.
- `it('every ref dropped by the backend: notifies "Nothing to blink" and closes')` (an empty answer).

`LibraryTab.test.tsx` (mock `BlinkViewer` the same way):

```tsx
it('blink_opens_only_frames_held_here', async () => {
  // three rows: u1 localState 'held', u2 'wanted', u3 'own_held'
  vi.mocked(api.invoke).mockImplementation(async (cmd: string) => {
    if (cmd === 'get_collab_blink_frames') return [blinkEntry('u1', 'replica')]; // u3 dropped by the backend
    return defaultInvoke(cmd); // the file's existing fallbacks
  });
  renderLibrary({ frames: [lib('u1', 'held'), lib('u2', 'wanted'), lib('u3', 'own_held')] });
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
  fireEvent.click(screen.getByRole('button', { name: 'Blink 2 of 3' }));
  await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_blink_frames', {
    projectId: 'p1', refs: [{ frameId: null, frameUuid: 'u1' }, { frameId: null, frameUuid: 'u3' }],
  }));
  await waitFor(() => expect(blink?.frames.map((f) => f.key)).toEqual(['u1']));
});
```

`MyFramesTab.test.tsx`: `it('each segment offers Blink with the eligible-subset label')`. Two ready rows, one with `path: null`; select all → `Blink 1 of 2`.

- [ ] **Step 2: Run to see them fail:** `npx vitest run src/components/collab/project/blinkEligibility.test.ts src/components/collab/project/ProjectBlink.test.tsx src/components/collab/project/LibraryTab.test.tsx src/components/collab/project/MyFramesTab.test.tsx`.

- [ ] **Step 3: Implement.**

`blinkEligibility.ts`:

```ts
import type { CollabFrameRef } from '../../../types/models';
import type { FrameVM } from './frames';

export type BlinkTable = 'ready' | 'review' | 'published' | 'held' | 'library';

/** Spec 2026-10-01 §9.4 — what Blink asks core for; null when the file is not on this device.
 *  Own frames go by catalog id (core picks raw / calibrated), library frames by uuid. */
export function blinkRef(v: FrameVM, table: BlinkTable): CollabFrameRef | null {
  if (table === 'library') {
    const ls = v.lib?.localState;
    return v.frameUuid && (ls === 'held' || ls === 'own_held') ? { frameId: null, frameUuid: v.frameUuid } : null;
  }
  const o = v.own;
  if (!o || v.frameId === null) return null;
  const byId = { frameId: v.frameId, frameUuid: null };
  switch (table) {
    case 'ready':
    case 'held':
      return o.path != null && !o.failures.some((f) => f.kind === 'blackHole') ? byId : null;
    case 'review':
      return (o.calibratedPath ?? o.path) != null ? byId : null;
    case 'published':
      return o.localState === 'own_held' || o.localState === 'own_changed' ? byId : null;
  }
}

export function blinkBadge(v: FrameVM): string | undefined {
  if (v.own?.withheld) return 'withheld';
  if (v.excluded) return 'excluded';
  if (v.own?.localState === 'own_changed' || v.lib?.localState === 'own_changed') return 'changed on disk';
  return undefined;
}

/** Mirrors `api::collab_blink` — an entry's key is its ref's uuid, else `f<frameId>`. */
export function refKey(r: CollabFrameRef): string {
  return r.frameUuid ?? `f${r.frameId}`;
}
```

`ProjectBlink.tsx`:

```tsx
import { useCallback, useEffect, useMemo, useState, type JSX } from 'react';
import { createPortal } from 'react-dom';
import { api } from '../../../api';
import BlinkViewer from '../../BlinkViewer';
import type { BlinkAction, BlinkFrame } from '../../blink/types';
import { useNotifications } from '../../../contexts/NotificationContext';
import { formatTimestamp } from '../../../utils/dateFormatting';
import type { CollabBlinkEntry, CollabFrameRef } from '../../../types/models';
import ExcludeDialog from './ExcludeDialog';
import { blinkBadge, blinkRef, refKey, type BlinkTable } from './blinkEligibility';
import type { FrameVM } from './frames';
import { useWithhold } from './useWithhold';

interface Row { e: CollabBlinkEntry; ref: CollabFrameRef; vmKey: string }

/** Spec 2026-10-01 §9 — Blink over a project table's selection, with the role's actions. */
export default function ProjectBlink({ projectId, table, vms, lookup, canModerate, onClose, onChanged }: {
  projectId: string; table: BlinkTable; vms: FrameVM[]; lookup: (vmKey: string) => FrameVM | undefined;
  canModerate: boolean; onClose: () => void; onChanged: () => void;
}): JSX.Element {
  const { notify } = useNotifications();
  const [rows, setRows] = useState<Row[] | null>(null);
  const [excluding, setExcluding] = useState<FrameVM[] | null>(null);
  const withhold = useWithhold(projectId, onChanged);

  // Resolved once per open: Blink snapshots its frames (spec §9.2).
  useEffect(() => {
    let cancelled = false;
    const picks = vms.flatMap((v) => {
      const ref = blinkRef(v, table);
      return ref ? [{ ref, vmKey: v.key }] : [];
    });
    const byKey = new Map(picks.map((p) => [refKey(p.ref), p]));
    api
      .invoke<CollabBlinkEntry[]>('get_collab_blink_frames', { projectId, refs: picks.map((p) => p.ref) })
      .then((got) => {
        if (cancelled) return;
        const next = got.flatMap((e) => {
          const p = byKey.get(e.key);
          return p ? [{ e, ref: p.ref, vmKey: p.vmKey }] : [];
        });
        if (next.length === 0) {
          console.warn('[projects] blink: none of the selection is on this device', { projectId, asked: picks.length });
          notify({ title: 'Nothing to blink', detail: 'None of these files is on this device.', kind: 'project', tone: 'warning' });
          onClose();
          return;
        }
        setRows(next);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error('[projects] get_collab_blink_frames failed:', err);
        notify({ title: 'Could not open Blink', detail: err instanceof Error ? err.message : String(err),
          kind: 'project', tone: 'warning', hasErrors: true });
        onClose();
      });
    return () => { cancelled = true; };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps -- once per open

  const vmKeyOf = useMemo(() => new Map((rows ?? []).map((r) => [r.e.key, r.vmKey])), [rows]);
  const vmOf = useCallback((f: BlinkFrame) => {
    const k = f.key ? vmKeyOf.get(f.key) : undefined;
    return k ? lookup(k) : undefined;
  }, [vmKeyOf, lookup]);

  const frames: BlinkFrame[] = useMemo(() => (rows ?? []).map(({ e, ref, vmKey }) => {
    const vm = lookup(vmKey);
    return {
      ...e.entry,
      key: e.key,
      source: e.source,
      imageRef: e.source === 'raw' ? undefined : { projectId, frame: ref },
      badge: vm ? blinkBadge(vm) : undefined,
    };
  }), [rows, lookup, projectId]);

  const own = table !== 'library';
  const viewOnly = !canModerate && (table === 'published' || table === 'library');
  const actions: BlinkAction[] = [];
  if (own) {
    actions.push({
      id: 'withhold', label: (n) => `Don't publish (${n})`, tone: 'warn',
      eligible: (f) => {
        const v = vmOf(f);
        return !!v?.own && (v.own.segment === 'ready' || v.own.segment === 'review') && !v.own.withheld;
      },
      run: async (fs) => {
        await withhold.dontPublish(fs.map((f) => {
          const v = vmOf(f)!;
          return { frameId: v.frameId!, prepared: v.own!.calibratedPath != null };
        }));
      },
    });
    actions.push({
      id: 'release', label: (n) => `Release (${n})`, tone: 'default',
      eligible: (f) => vmOf(f)?.own?.withheld === true,
      run: async (fs) => { await withhold.release(fs.map((f) => vmOf(f)!.frameId!)); },
    });
  }
  if (canModerate) {
    actions.push({
      id: 'exclude', label: (n) => `Exclude from project (${n})`, tone: 'danger',
      eligible: (f) => {
        const v = vmOf(f);
        return !!v && v.pubState === 'published' && !v.excluded && v.frameUuid !== null
          && (own ? v.own?.segment === 'published' : true);
      },
      run: (fs) => setExcluding(fs.map((f) => vmOf(f)!).filter(Boolean)),
    });
  }

  const contextLabel = (f: BlinkFrame): string => {
    const v = vmOf(f);
    const parts: string[] = [];
    if (f.source === 'replica') {
      const who = v?.lib?.receivedFromMember ?? v?.publisher ?? 'a member';
      const at = v?.lib?.receivedAt;
      parts.push(`received from ${who}${at ? ` · ${formatTimestamp(at, { seconds: true })}` : ''}`);
    } else if (f.source === 'calibrated') {
      parts.push(v?.own?.segment === 'review' ? 'exactly the file that will be published' : 'the calibrated file of this frame');
    } else {
      parts.push('the raw frame on this device');
    }
    if (viewOnly) {
      parts.push(table === 'published' ? 'Only a moderator can exclude published frames.' : 'Only a moderator can exclude frames.');
    }
    return parts.join(' · ');
  };

  return (
    <>
      {rows && createPortal(
        <BlinkViewer frames={frames} onClose={onClose} actions={actions} contextLabel={contextLabel} viewOnly={viewOnly} />,
        document.body,
      )}
      {withhold.dialog}
      {excluding && (
        <ExcludeDialog projectId={projectId} frames={excluding} onClose={() => setExcluding(null)} onDone={() => onChanged()} />
      )}
    </>
  );
}
```

Blink sits in a body portal, so a dialog opened later (each `DialogShell` portals to body too) lands after it in the DOM and paints above it at the same `z-50`. The overlay stack gives that dialog Escape and focus (Task 7). A frame of the Held back table may come back `calibrated` (a wave-1 leftover: a prepared frame now failing the gate). The strip names the entry's real `source`, so the label stays honest.

`MyFramesTab.tsx`:

```ts
const [blinking, setBlinking] = useState<{ table: BlinkTable; vms: FrameVM[] } | null>(null);
const ownByKey = useMemo(() => new Map((rows ?? []).map((r) => [ownFrameKey(r), fromOwn(r)] as const)), [rows]);
const lookup = useCallback((k: string) => ownByKey.get(k), [ownByKey]);
const blinkAction = (table: BlinkTable): TableAction => ({
  id: 'blink', verb: 'Blink', eligible: (v) => blinkRef(v, table) !== null, run: (t) => setBlinking({ table, vms: t }),
});
```

- Append `blinkAction('ready')`, `('review')`, `('published')` and `('held')` as the last action of each table, as in the mockup.
- Render `{blinking && <ProjectBlink projectId={projectId} table={blinking.table} vms={blinking.vms} lookup={lookup} canModerate={canModerate} onClose={() => setBlinking(null)} onChanged={onReload} />}`.

`LibraryTab.tsx`:
- The same `blinkAction('library')` is the last action.
- `lookup` reads the tab's existing library `FrameVM[]` memo by `key`.
- `onChanged={reload}`, and `canModerate` comes from props.

- [ ] **Step 4: Run:** `npx tsc --noEmit -p . && npx vitest run src/components/collab/project/ src/components/BlinkViewer.test.tsx` → green.
- [ ] **Step 5: Commit** `feat(collab-ui): Blink the selection from My frames and Library — frames on this device, role actions, Exclude over Blink`.

---

### Task 9: Harness, docs, amendments, smoke list, final gates

**Files:**
- Modify: `scripts/ui-harness/fixtures.mjs` and `scripts/ui-harness/README.md`
- Modify: `docs/frontend/notifications.md`
- Modify: `docs/superpowers/specs/2026-10-01-collab-publish-review-design.md` (§16: wave-2 amendments)
- Modify: `docs/transfers/README.md` (the collab project page paragraph)
- Modify: `docs/superpowers/open-items.md`
- Modify: `CLAUDE.md`, only if the check in Step 4 finds no publish-review line

- [ ] **Step 1: Harness fixtures.** In `buildFixtures(scenario)`:
  - The project card gains `publishMode: 'manual'` and `syncedAt: new Date(Date.now() - 8000).toISOString()`.
  - Every own row gains `calibratedPath: null, calibratedBytes: null, preparedAt: null, withheld: false`.
  - Counts gain `prepared: 0, withheld: 0`.
  - New handlers:
    - `get_collab_publish_run` → `{ running: null, last: null }`;
    - `cancel_collab_publish` → `null`;
    - `calibrate_collab_frames` and `publish_collab_frames` → a `PublishResult` with zeros;
    - `set_collab_frames_withheld` → `args.frameIds.length`;
    - `set_project_publish_mode` → `null`;
    - `get_collab_blink_frames` → `[]`;
    - `get_collab_frame_image` → `null`.
  - A new `review` scenario:
    - 48 own rows in `segment: 'review'` with `calibratedPath: '/collab/own/c_<name>'`, `calibratedBytes: 44 * 1024 * 1024` and a `preparedAt`, each with `exptimeSec: 300` and filters Hα / OIII.
    - Three held rows with `withheld: true` and failure `{ kind: 'withheld', text: 'Withheld by you' }`.
    - `get_collab_publish_run` → `{ running: { kind: 'calibrate', trigger: 'manual', mode: null, stage: 'calibrating', current: 12, total: 48, currentFile: 'M42_Ha_300s_0012.fits', publishRunId: 'h1', projectId: PID, startedAt: <now − 134 s> }, last: null }`.
  - Add the scenario to the README's list.

  Start it: `HARNESS_SCENARIO=review npm run ui:harness`. Open `/projects/<PID>?tab=mine`. The run panel, the four tiles and the To review table render, and the harness log prints no `unknown command` for any command above. Stop the harness.

- [ ] **Step 2: `docs/frontend/notifications.md`.**
  - Replace the `collab-published` entry with `collab-publish-finished`: the outcome table of spec §5.4 as Task 2 implemented it, with F8.
  - Add "Sync did not complete — {error}" / "— no answer from the hub" (the Live pill, `CollabLiveStatus`) and "Could not open Blink" / "Nothing to blink" (`ProjectBlink`).
  - State that `usePublishing` no longer notifies run outcomes (F4).

- [ ] **Step 3: Spec §16, wave-2 amendments.** Add a "Wave 2 (frontend)" sub-table:
  - rulings F1–F9, verbatim from this plan;
  - `BlinkFrame.imageRef.frame` (not `.ref`, W8);
  - the `contextLabel` signature `(f) => string`;
  - `SegmentTiles.sub`;
  - every deviation the task reports recorded (take them from the ledger).

  One line each, the same table style as the wave-1 rows.

- [ ] **Step 4: `docs/transfers/README.md` and `CLAUDE.md`.** In the transfers reference's collab project page section, add one paragraph covering:
  - the Overview order (My contribution, then the Project settings card);
  - the My frames To review segment and the run panel (`useCollabPublishRun`);
  - the Live pill confirmation (`collab-project-synced`);
  - project Blink (`ProjectBlink`, `BlinkViewer` project mode with no Black Hole).

  Then `grep -n "publish-review\|To review\|publishMode" CLAUDE.md`. If the collab bullets do not mention the review model yet, add ONE bullet under Transfers / personal sync: **Publishing is Calibrate → Review → Publish (spec 2026-10-01)**: modes `manual` (default, migrated) / `autoCalibrate` / `automatic`; Don't publish = a local withhold; the Black Hole is never a project action; Blink in a project takes caller actions. The command count is unchanged by this wave.

- [ ] **Step 5: `docs/superpowers/open-items.md`.** Add a "Collab publish review — owner smoke (wave 2)" block with spec §12's real-app list (items 1–7). Add:
  8. a side-by-side of the Overview, My frames and Blink against the canvas artboards, in a real window at 1440 px and at a phone-narrow width;
  9. dark and light themes.

  Add the release-note line owed: "Publishing now has a review step: Calibrate → review in Blink → Publish. Auto-publish is off for every project (migrated to Manual); Auto-calibrate and Fully automatic are opt-in per project."

- [ ] **Step 6: Final gates.**
  - `npx tsc --noEmit -p .` → 0 errors.
  - `npx vitest run` → all pass; record the count against Task 0's baseline.
  - `grep -rn "collab-published\|set_project_auto_publish\|autoPublish" src` → no hits outside comments that explain the retirement.
  - `grep -rln "@tauri-apps" src | grep -v "^src/api/"` → empty.
- [ ] **Step 7: Commit** `docs(collab): publish review wave 2 — harness review scenario, notifications, amendments, smoke list`.
