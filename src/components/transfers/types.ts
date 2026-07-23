// Shared types for the unified master-detail Transfers list (Transfers Status
// Model v2, §D8). The list is a single stream of BOTH live rows (from
// `useTransferQueue`) and completed/failed history groups (from
// `useTransferHistory`) — "Completed is a filter over the same list", so both
// sources flow through one row model and one selection key.

import type { Direction } from '../../types/models';
import type { TransferRow } from '../../hooks/useTransferQueue';
import type { HistoryGroup } from './historyGrouping';

/** The filter chips over the unified list (§D8). `all` matches everything.
 *  `cancelled` sits between `completed` and `failed` (UX wave 2). */
export type TransferFilter =
  | 'all'
  | 'sending'
  | 'receiving'
  | 'waiting'
  | 'completed'
  | 'cancelled'
  | 'failed';

/** The batch identity `delete_transfer_history` deletes by (UX wave 2). `batchUuid`
 *  in BOTH directions (B5b unified the received side onto it, symmetric with sent —
 *  for sent it's the package-dir basename == the `sent` history `packageId`; for
 *  received it's `sync_inbound.batch_uuid` == the `received` history `packageId`).
 *  The received wire `packageId` still rotates per attempt but is no longer the
 *  delete key. `null` on a row that cannot be deleted (a non-terminal live row, or a
 *  legacy "Earlier transfers" history bucket with no single package key). */
export interface DeleteKey {
  direction: Direction;
  packageKey: string;
}

/** One entry in the unified list: either a live in-flight/ledger row or a
 *  merged-in completed history group. `selKey` is the stable selection handle
 *  (namespaced — live `out:`/`in:` vs history `sent:`/`received:` — so they
 *  never collide). */
export type UnifiedRow =
  | {
      kind: 'live';
      selKey: string;
      row: TransferRow;
      /** Batch delete key (trash action), or `null` on a non-terminal live row.
       *  Batch model: one row per transfer, so there is nothing to collapse — the
       *  "attempt N" hint reads `row.generation`, not a collapsed count. */
      deleteKey: DeleteKey | null;
    }
  | {
      kind: 'history';
      selKey: string;
      group: HistoryGroup;
      /** Resolved friendly device name, or `null` → fall back to short hex. */
      deviceName: string | null;
      /** Resolved sender device capability (`"athenaeum"` | `"perseus"`) for a
       *  RECEIVED group's origin badge, or `null` (unknown / a sent group → no
       *  badge). A live inbound row uses `TransferRow.peerKind` instead. */
      deviceKind: string | null;
      /** Resolved collab project title, or `null`. */
      projectName: string | null;
      /** Batch delete key (trash action), or `null` for a legacy null-key
       *  "Earlier transfers" bucket that has no single package key. */
      deleteKey: DeleteKey | null;
    };
