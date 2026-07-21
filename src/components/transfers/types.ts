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

/** The batch identity `delete_transfer_history` deletes by (UX wave 2). For a
 *  sent batch it is the full package-dir basename (== the `sent` history
 *  `packageId`); for a received batch it is the wire package uuid. `null` on a
 *  row that cannot be deleted (an active attempt, or a sent terminal row whose
 *  full basename can't be resolved from history — the live row only carries the
 *  truncated `packageShort`). */
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
      /** How many DB-terminal attempts of this batch collapsed into this row
       *  (≥1). `>1` renders the muted "· N attempts" hint. Always `1` for an
       *  active (non-terminal) row — those are never collapsed. */
      attemptCount: number;
      /** Batch delete key (trash action), or `null` when the row can't be
       *  deleted (active, or an unresolvable sent terminal row). */
      deleteKey: DeleteKey | null;
    }
  | {
      kind: 'history';
      selKey: string;
      group: HistoryGroup;
      /** Resolved friendly device name, or `null` → fall back to short hex. */
      deviceName: string | null;
      /** Resolved collab project title, or `null`. */
      projectName: string | null;
      /** Batch delete key (trash action), or `null` for a legacy null-key
       *  "Earlier transfers" bucket that has no single package key. */
      deleteKey: DeleteKey | null;
    };
