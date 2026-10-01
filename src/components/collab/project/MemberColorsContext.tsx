import { createContext, useCallback, useContext, useMemo, type ReactNode } from 'react';
import { memberColor } from './memberColors';

/** A member's colour, or `undefined` for a key that names no known member
 *  (null, an unnamed device, a departed member): those render neutral, never
 *  in the palette's slot 0, which is this account's accent. */
type Lookup = (accountIdOrName: string | null) => string | undefined;
const Ctx = createContext<Lookup>(() => undefined);

/** Spec §4.4 — one member → colour lookup for the whole project page. */
export function MemberColorsProvider({ members, selfAccountId, children }: {
  members: { accountId: string; displayName: string }[];
  selfAccountId: string | null;
  children: ReactNode;
}) {
  const byName = useMemo(() => new Map(members.map((m) => [m.displayName, m.accountId])), [members]);
  const lookup = useCallback<Lookup>(
    (key) => {
      if (!key) return undefined;
      const known = key === selfAccountId || members.some((m) => m.accountId === key);
      const accountId = known ? key : byName.get(key);
      return accountId ? memberColor(accountId, members, selfAccountId) : undefined;
    },
    [members, byName, selfAccountId],
  );
  return <Ctx.Provider value={lookup}>{children}</Ctx.Provider>;
}

export function useMemberColor(): Lookup {
  return useContext(Ctx);
}
