import { createContext, useContext, ReactNode } from 'react';
import { useAnalysisProgress } from '../hooks/useAnalysisProgress';

type AnalysisProgressContextType = ReturnType<typeof useAnalysisProgress>;

const AnalysisProgressContext = createContext<AnalysisProgressContextType | null>(null);

export function AnalysisProgressProvider({ children }: { children: ReactNode }) {
  const value = useAnalysisProgress();
  return (
    <AnalysisProgressContext.Provider value={value}>
      {children}
    </AnalysisProgressContext.Provider>
  );
}

export function useAnalysisProgressContext() {
  const ctx = useContext(AnalysisProgressContext);
  if (!ctx) {
    throw new Error('useAnalysisProgressContext must be used within AnalysisProgressProvider');
  }
  return ctx;
}
