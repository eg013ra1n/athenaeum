import { createContext, useContext, ReactNode } from 'react';
import { useStackingRuns } from '../hooks/useStackingRuns';

type StackingContextType = ReturnType<typeof useStackingRuns>;

const StackingContext = createContext<StackingContextType | null>(null);

export function StackingProvider({ children }: { children: ReactNode }) {
  const value = useStackingRuns();
  return (
    <StackingContext.Provider value={value}>
      {children}
    </StackingContext.Provider>
  );
}

export function useStackingContext() {
  const ctx = useContext(StackingContext);
  if (!ctx) {
    throw new Error('useStackingContext must be used within StackingProvider');
  }
  return ctx;
}
