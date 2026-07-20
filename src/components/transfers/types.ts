// Shared types for the unified master-detail Transfers list (Transfers Status
// Model v2, §D8). The list is a single stream of BOTH live rows (from
// `useTransferQueue`) and completed/failed history groups (from
// `useTransferHistory`) — "Completed is a filter over the same list", so both
// sources flow through one row model and one selection key.

import type { TransferRow } from '../../hooks/useTransferQueue';
import type { HistoryGroup } from './historyGrouping';

/** The six filter chips over the unified list (§D8). `all` matches everything. */
export type TransferFilter = 'all' | 'sending' | 'receiving' | 'waiting' | 'completed' | 'failed';

/** One entry in the unified list: either a live in-flight/ledger row or a
 *  merged-in completed history group. `selKey` is the stable selection handle
 *  (namespaced — live `out:`/`in:` vs history `sent:`/`received:` — so they
 *  never collide). */
export type UnifiedRow =
  | {
      kind: 'live';
      selKey: string;
      row: TransferRow;
    }
  | {
      kind: 'history';
      selKey: string;
      group: HistoryGroup;
      /** Resolved friendly device name, or `null` → fall back to short hex. */
      deviceName: string | null;
      /** Resolved collab project title, or `null`. */
      projectName: string | null;
    };
