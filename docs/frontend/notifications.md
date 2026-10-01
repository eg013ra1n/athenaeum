# Notifications

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


One global notification system. **To raise a notification from anywhere, call
`notify()` from `useNotifications()` (`src/contexts/NotificationContext.tsx`)** —
do not build ad-hoc toasts/banners.

```ts
const { notify } = useNotifications();
notify({
  title: 'Scan finished — 12 new or updated',
  detail: '4231 on disk, 4219 unchanged',
  kind: 'scan',          // NotificationKind → drives the panel icon
  tone: 'success',       // 'info' | 'warning' | 'success' (toast colour)
  hasErrors: false,      // true → error styling
  link: '/about',        // optional in-app route; entry/toast becomes clickable
  toast: true,           // default true; false = history entry only, no toast
  dedupeKey: 'scan-42',  // optional; suppress duplicates with the same key
});
```

- `notify` adds a **persistent history entry** (notification panel, opened from
  the sidebar bell) and, unless `toast:false`, a 5s **toast**. History +
  dedupe set persist to `localStorage` (`athenaeum.notifications.v1`, capped;
  corrupt data is ignored, never throws). The bell shows the unread count;
  opening the panel marks all read.
- **Surface**: `NotificationPanel` (slide-over) is rendered at app root in
  `Layout.tsx` so it is not clipped by the sidebar. `NotificationBell` only
  calls `openPanel()`. `ToastStack` renders transient toasts.
- **`NotificationKind`** (icon map lives in `NotificationPanel.tsx`): `files`,
  `update`, `merge`, `scan`, `export`, `analysis`, `platesolve`, `autofind`,
  `archive`, `fileop`, `registration`, `masterbuild`, `calibration`,
  `stacking`, `sync`, `project`, `generic` (the union in
  `src/contexts/NotificationContext.tsx`). Add a kind → add it to the union
  *and* the icon map.
- **Backend events → notifications**: don't add a listener in
  `NotificationContext`. Call `notify()` from the existing completion handler in
  the relevant hook/component (pattern: `useScanProgress`, `useExportProgress`,
  `useAnalysisProgress`, `usePlateSolveQueue`, `FillObjectsPanel`,
  `ArchiveProgress`, `DualPaneFileBrowser`). Notify on **discrete outcomes**
  only — never on `*-progress` (high-frequency). Use `dedupeKey` (e.g. an
  operation id) when the handler can fire more than once.
- **Tauri/SSE listener pattern (required, StrictMode-safe).** `api.listen` is
  async; React 18 StrictMode double-mounts in dev. If you `await` the unlisten
  into a variable, the cleanup can run before it resolves → a **leaked second
  listener** (double events). Always use the cancelled-flag form:

  ```ts
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<T>('event', (p) => { if (cancelled) return; handle(p); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[X] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  ```

- Timestamps: `formatTimestamp` from `src/utils/dateFormatting.ts`
  (`YYYY-MM-DD HH:MM`). Don't re-implement.

## Collab publishing and sync (2026-10-02)

Spec `docs/superpowers/specs/2026-10-01-collab-publish-review-design.md` §5.4 and §16.1.
`collab-published` is retired; a started publish-family run is notified in **one** place,
`collab-publish-finished` in `src/hooks/useCollabNotifications.ts`, mounted once at the app root.
`usePublishing` no longer notifies run outcomes (F4): it keeps its inline error line, the
`updateRequired` banner, the A6 refusal box and the busy info toast for refusals returned by the
command before a run starts.

| Outcome of `collab-publish-finished` | Title | Link |
| ---- | ---- | ---- |
| `done`, nothing sent, calibrated > 0 (a calibrate run, or an `auto` run in Auto-calibrate mode, F8) | "Calibrated n frames in {title} — review them" (success; `hasErrors` when frames were held back) | `/projects/{id}?tab=mine&segment=review` |
| `done`, announced + updated > 0 | "Published n frames in {title}" (detail: n new · m updated, plus held back) | `?tab=mine&segment=published` |
| `done`, nothing sent or calibrated, held back > 0 | "Nothing new to publish in {title}" (warning) | `?tab=mine&segment=held` |
| `done`, all counts zero | silent | none |
| `cancelled` | "Stopped in {title}" (info, history only: `toast: false`) | `?tab=mine` |
| `refused` | "{device} publishes {title}" for an A6 refusal (`collab_publishing_device:<name>`, F8), otherwise "Not published in {title}" with the error; toast only for a manual run, history only for an auto run | `?tab=mine` |
| `failed` | "Publishing failed in {title}" + the error, `hasErrors` | `?tab=mine` |

The `segment` URL parameter is handled like `tab` (applied, then removed from the URL).

Other collab outcomes notified from the page, each after a `console.error` / `console.warn`:

| Where | Title |
| ---- | ---- |
| Live pill, `CollabLiveStatus` (waits for `collab-project-synced`) | "Sync did not complete — {error}" when the hub refuses the project; "Sync did not complete — no answer from the hub" after the wait times out; "Sync now failed" when `collab_sync_now` itself errors |
| `ProjectBlink` | "Could not open Blink" (the call failed); "Nothing to blink" (none of the selection is on this device) |
| `useWithhold` (Don't publish / Release) | "Could not withhold the frames" / "Could not release the frames" |

Toasts show titles only, so the failure reason is part of the Live pill's title.

