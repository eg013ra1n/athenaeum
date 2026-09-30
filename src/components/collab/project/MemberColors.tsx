import { createContext, useCallback, useContext, useMemo, type ReactNode } from 'react';
import { MEMBER_PALETTE, memberColor } from './memberColors';

type Lookup = (accountIdOrName: string | null) => string;
const Ctx = createContext<Lookup>(() => MEMBER_PALETTE[0]);

/** Spec §4.4 — one member → colour lookup for the whole project page. */
export function MemberColorsProvider({ members, selfAccountId, children }: {
  members: { accountId: string; displayName: string }[];
  selfAccountId: string | null;
  children: ReactNode;
}) {
  const byName = useMemo(() => new Map(members.map((m) => [m.displayName, m.accountId])), [members]);
  const lookup = useCallback<Lookup>(
    (key) => {
      if (!key) return MEMBER_PALETTE[0];
      const accountId = members.some((m) => m.accountId === key) ? key : byName.get(key);
      return accountId ? memberColor(accountId, members, selfAccountId) : MEMBER_PALETTE[0];
    },
    [members, byName, selfAccountId],
  );
  return <Ctx.Provider value={lookup}>{children}</Ctx.Provider>;
}

export function useMemberColor(): Lookup {
  return useContext(Ctx);
}
