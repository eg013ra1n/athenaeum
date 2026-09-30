/**
 * Short chip labels for a `ContributorState` key (spec §8.1–§8.2). Shared by
 * the collab project page's frame tables and `LightsAnalysisTable`'s
 * per-frame Project column (Task 9) so the surfaces read as one system.
 */
export const SHORT: Record<string, string> = {
  notPublished: '—',
  failsGate: 'fails gate',
  pendingApproval: 'pending',
  published: 'published',
  updatePending: 'update',
  rejected: 'rejected',
  publishedNotOnDisk: 'not on disk',
  publishedNowFailsGate: 'now fails',
};
