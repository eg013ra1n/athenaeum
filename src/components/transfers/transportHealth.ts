import type { TransportHealth } from '../../types/models';

/**
 * Presentational view of the transport-health surface (Task 3.3), shared by the
 * sidebar `TransferIndicator` dot and the `TransfersPanel` header line so both
 * map the four `TransportHealth.status` values to the SAME colour + copy.
 *
 * Colour mapping (design tokens, per the owner's decision): relay connected →
 * success, direct-only → warning, no relay map → error, not started → muted.
 */
export interface TransportHealthView {
  /** Tailwind design-token bg class for the small status dot. */
  dot: string;
  /** Short label (e.g. for a compact chip). */
  label: string;
  /** One-line human explanation for the badge `title` and the panel line. */
  detail: string;
}

export function transportHealthView(t: TransportHealth): TransportHealthView {
  switch (t.status) {
    case 'relay_connected':
      return {
        dot: 'bg-success',
        label: 'Relay connected',
        detail: t.relayUrl
          ? `Relay connected — reachable via ${t.relayUrl}`
          : 'Relay connected — reachable by remote peers',
      };
    case 'direct_only':
      return {
        dot: 'bg-warning',
        label: 'Direct only',
        detail:
          'Direct connections only — peers behind NAT may be unreachable' +
          (t.lastError ? ` (${t.lastError})` : ''),
      };
    case 'no_relay_map':
      return {
        dot: 'bg-error',
        label: 'No relay configuration',
        detail: 'No relay configuration — transfers to remote peers will stall',
      };
    default: // 'not_started' (and any unexpected value)
      return {
        dot: 'bg-content-muted',
        label: 'Not started',
        detail: 'Transport not started yet',
      };
  }
}
