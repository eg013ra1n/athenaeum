import { useCallback, useRef, useState } from 'react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import type { ProjectCard, PublishResult } from '../../../types/models';

/** A hub call refused this build with the stable `collab_api_outdated`
 *  prefix (P17). */
function isOutdated(msg: string): boolean {
  return msg.startsWith('collab_api_outdated');
}

/** The backend's refusal while another publish run of the same project
 *  (manual, republish or the background auto-publish) is in progress —
 *  `api::collab::PUBLISH_BUSY_MSG`, owner decision 2026-09-24: refused,
 *  never queued. */
const PUBLISH_BUSY = 'publication of this project is already running';

function isPublishBusy(msg: string): boolean {
  return msg.includes(PUBLISH_BUSY);
}

/** Inline text + toast for a busy refusal: not a failure of the user's
 *  data, just "try again once the running one ends". */
const PUBLISH_BUSY_INLINE =
  'Publication of this project is already running — wait for it to finish, then try again.';

/** Amendment A6: another device of this account is the project's publishing
 *  device. A publish/republish rejects with
 *  `collab_publishing_device:<name>` (`account::client::publishing_device_msg`)
 *  when nothing else went out; the prefix is stable, the rest is the device
 *  name or `OTHER_DEVICE`. */
const PUBLISHING_DEVICE_PREFIX = 'collab_publishing_device:';

/** The name core uses for a bound device the hub reports without one
 *  (`account::client::publishing_device_label`). */
export const OTHER_DEVICE = 'another device of this account';

export function publishingDeviceRefusal(msg: string): string | null {
  if (!msg.startsWith(PUBLISHING_DEVICE_PREFIX)) return null;
  return msg.slice(PUBLISHING_DEVICE_PREFIX.length).trim() || OTHER_DEVICE;
}

/** A run that ALSO posted versions resolves Ok, with the refused new frames
 *  in `heldBack` carrying `publishingDevice` — the bound device's name (or
 *  `OTHER_DEVICE`). Keyed on that field only, never on the reason text. */
function heldForPublishingDevice(res: PublishResult | null | undefined): string | null {
  for (const frame of res?.heldBack ?? []) {
    if (frame.publishingDevice != null) return deviceLabel(frame.publishingDevice);
  }
  return null;
}

/** The bound device's name for display; never "this device". */
export function deviceLabel(name: string | null | undefined): string {
  return name?.trim() || OTHER_DEVICE;
}

/** A device name at the start of a sentence. */
export function leading(name: string): string {
  return name === OTHER_DEVICE ? 'Another device of this account' : name;
}

export interface PublishingOptions {
  /** Re-read `get_collab_project_detail` (the card, links). */
  reloadDetail: () => Promise<void>;
  /** Re-read `list_project_own_frames`. */
  reloadOwn: () => Promise<void>;
  /** The card `set_collab_publishing_device` answered with — applied as is,
   *  no reload needed. */
  onCard: (card: ProjectCard) => void;
}

export interface Publishing {
  publish: (frameIds: number[]) => Promise<void>;
  republish: (frameIds: number[] | null) => Promise<void>;
  /** Calibrate the frames for review; opens no confirm. */
  calibrate: (frameIds: number[]) => Promise<void>;
  switchHere: () => Promise<void>;
  publishBusy: boolean;
  publishError: string | null;
  republishBusy: boolean;
  republishError: string | null;
  calibrateBusy: boolean;
  calibrateError: string | null;
  switchBusy: boolean;
  /** The publishing device a publish/republish was refused for (A6). */
  refusedBy: string | null;
  updateRequired: boolean;
  clearPublishError: () => void;
  clearRepublishError: () => void;
}

/**
 * The project page's publish / republish / calibrate / "Publish from this
 * device" orchestration. `publish_collab_frames` and
 * `republish_collab_frames` carry `frameIds`. `republish` still accepts
 * `null` (the command's "all"), but the project page never passes it: its
 * guard sends the published, non-excluded ids it counted, because a null
 * republish runs over every gate candidate and would announce never-published
 * Ready frames.
 *
 * F4: the app-root `useCollabNotifications` hook is the one place that turns
 * `collab-publish-finished` (the outcome of every STARTED run) into a toast.
 * `publish`/`republish`/`calibrate` do their own local UI work (reload own
 * frames + detail) and show a refusal returned before any run starts inline
 * in My frames (error line, `updateRequired`, the A6 box); only a busy
 * refusal also raises an info toast, since no run exists to finish. The
 * shell closes its confirm the moment the user confirms (final-review
 * ruling, spec §16.1), so the run panel and its Cancel stay reachable for
 * the whole run — nothing here keeps a dialog open.
 */
export function usePublishing(projectId: string | undefined, options: PublishingOptions): Publishing {
  const { notify } = useNotifications();
  // Latest-value ref: the shell passes inline callbacks.
  const opts = useRef(options);
  opts.current = options;

  const [publishBusy, setPublishBusy] = useState(false);
  const [publishError, setPublishError] = useState<string | null>(null);
  const [republishBusy, setRepublishBusy] = useState(false);
  const [republishError, setRepublishError] = useState<string | null>(null);
  const [calibrateBusy, setCalibrateBusy] = useState(false);
  const [calibrateError, setCalibrateError] = useState<string | null>(null);
  const [refusedBy, setRefusedBy] = useState<string | null>(null);
  const [switchBusy, setSwitchBusy] = useState(false);
  const [updateRequired, setUpdateRequired] = useState(false);

  /** A6 refusal: the other device publishes new frames here. Inline with
   *  the switch action; the finished event carries the notification (F4). The
   *  reload picks up the binding core recorded. */
  const showPublishingDeviceRefusal = (name: string) => {
    setRefusedBy(name);
    void opts.current.reloadDetail();
  };

  /** "Publish from this device": moves the binding here (the shell confirms
   *  first). */
  const switchHere = async () => {
    if (!projectId) return;
    setSwitchBusy(true);
    try {
      const card = await api.invoke<ProjectCard>('set_collab_publishing_device', { projectId });
      opts.current.onCard(card);
      setRefusedBy(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] set publishing device failed:', err);
      if (isOutdated(msg)) {
        setUpdateRequired(true);
      } else {
        notify({
          title: 'Could not publish from this device',
          detail: msg,
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link: `/projects/${projectId}`,
          dedupeKey: `publishing-device-switch-failed-${projectId}-${Date.now()}`,
        });
      }
    } finally {
      setSwitchBusy(false);
    }
  };

  const publish = async (frameIds: number[]) => {
    if (!projectId) return;
    setPublishBusy(true);
    setPublishError(null);
    try {
      const res = await api.invoke<PublishResult>('publish_collab_frames', { projectId, frameIds });
      setRefusedBy(heldForPublishingDevice(res));
      await opts.current.reloadOwn();
      await opts.current.reloadDetail();
    } catch (err) {
      // F4 — a refusal before any run: logged, then shown inline in My frames
      // (the error line, the `updateRequired` banner or the A6 box); only a
      // busy refusal also toasts. A started run's failure is notified once,
      // from `collab-publish-finished`.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] publish failed:', err);
      const refused = publishingDeviceRefusal(msg);
      if (isOutdated(msg)) {
        setUpdateRequired(true);
      } else if (refused) {
        showPublishingDeviceRefusal(refused);
      } else if (isPublishBusy(msg)) {
        setPublishError(PUBLISH_BUSY_INLINE);
        notify({
          title: 'Publication already running',
          detail: 'Wait for the current run of this project to finish, then publish again.',
          kind: 'project',
          tone: 'info',
          link: `/projects/${projectId}`,
          dedupeKey: `publish-busy-${projectId}-${Date.now()}`,
        });
      } else {
        // F4: inline only. A started run that fails is notified once, from
        // `collab-publish-finished`; this is a refusal before any run.
        setPublishError(msg);
      }
    } finally {
      setPublishBusy(false);
    }
  };

  const republish = async (frameIds: number[] | null) => {
    if (!projectId) return;
    setRepublishBusy(true);
    setRepublishError(null);
    try {
      const res = await api.invoke<PublishResult>('republish_collab_frames', { projectId, frameIds });
      setRefusedBy(heldForPublishingDevice(res));
      await opts.current.reloadOwn();
      await opts.current.reloadDetail();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] republish failed:', err);
      const refused = publishingDeviceRefusal(msg);
      if (isOutdated(msg)) {
        setUpdateRequired(true);
      } else if (refused) {
        showPublishingDeviceRefusal(refused);
      } else if (isPublishBusy(msg)) {
        setRepublishError(PUBLISH_BUSY_INLINE);
        notify({
          title: 'Publication already running',
          detail: 'Wait for the current run of this project to finish, then republish again.',
          kind: 'project',
          tone: 'info',
          link: `/projects/${projectId}`,
          dedupeKey: `republish-busy-${projectId}-${Date.now()}`,
        });
      } else {
        setRepublishError(msg);
      }
    } finally {
      setRepublishBusy(false);
    }
  };

  const calibrate = async (frameIds: number[]) => {
    if (!projectId) return;
    setCalibrateBusy(true);
    setCalibrateError(null);
    try {
      const res = await api.invoke<PublishResult>('calibrate_collab_frames', { projectId, frameIds });
      setRefusedBy(heldForPublishingDevice(res));
      await opts.current.reloadOwn();
      await opts.current.reloadDetail();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] calibrate failed:', err);
      const refused = publishingDeviceRefusal(msg);
      if (isOutdated(msg)) {
        setUpdateRequired(true);
      } else if (refused) {
        showPublishingDeviceRefusal(refused);
      } else if (isPublishBusy(msg)) {
        setCalibrateError(PUBLISH_BUSY_INLINE);
        notify({
          title: 'Publication already running',
          detail: 'Wait for the current run of this project to finish, then calibrate again.',
          kind: 'project',
          tone: 'info',
          link: `/projects/${projectId}`,
          dedupeKey: `calibrate-busy-${projectId}-${Date.now()}`,
        });
      } else {
        setCalibrateError(msg);
      }
    } finally {
      setCalibrateBusy(false);
    }
  };

  const clearPublishError = useCallback(() => setPublishError(null), []);
  const clearRepublishError = useCallback(() => setRepublishError(null), []);

  return {
    publish,
    republish,
    calibrate,
    switchHere,
    publishBusy,
    publishError,
    republishBusy,
    republishError,
    calibrateBusy,
    calibrateError,
    switchBusy,
    refusedBy,
    updateRequired,
    clearPublishError,
    clearRepublishError,
  };
}
