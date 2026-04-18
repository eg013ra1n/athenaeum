import { createContext, useContext, ReactNode } from 'react';
import { usePlateSolveQueue } from '../hooks/usePlateSolveQueue';

type PlateSolveProgressContextType = ReturnType<typeof usePlateSolveQueue>;

const PlateSolveProgressContext = createContext<PlateSolveProgressContextType | null>(null);

export function PlateSolveProgressProvider({ children }: { children: ReactNode }) {
  const value = usePlateSolveQueue();
  return (
    <PlateSolveProgressContext.Provider value={value}>
      {children}
    </PlateSolveProgressContext.Provider>
  );
}

export function usePlateSolveProgressContext() {
  const ctx = useContext(PlateSolveProgressContext);
  if (!ctx) {
    throw new Error('usePlateSolveProgressContext must be used within PlateSolveProgressProvider');
  }
  return ctx;
}
