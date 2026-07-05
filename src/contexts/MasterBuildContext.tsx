import { createContext, useContext, ReactNode } from 'react';
import { useMasterBuilds } from '../hooks/useMasterBuilds';

type MasterBuildContextType = ReturnType<typeof useMasterBuilds>;

const MasterBuildContext = createContext<MasterBuildContextType | null>(null);

export function MasterBuildProvider({ children }: { children: ReactNode }) {
  const value = useMasterBuilds();
  return (
    <MasterBuildContext.Provider value={value}>
      {children}
    </MasterBuildContext.Provider>
  );
}

export function useMasterBuildContext() {
  const ctx = useContext(MasterBuildContext);
  if (!ctx) {
    throw new Error('useMasterBuildContext must be used within MasterBuildProvider');
  }
  return ctx;
}
