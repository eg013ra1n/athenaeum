import { createContext, useContext, ReactNode } from 'react';
import { useScanProgress } from '../hooks/useScanProgress';

type ScanProgressContextType = ReturnType<typeof useScanProgress>;

const ScanProgressContext = createContext<ScanProgressContextType | null>(null);

export function ScanProgressProvider({ children }: { children: ReactNode }) {
  const scanProgress = useScanProgress();

  return (
    <ScanProgressContext.Provider value={scanProgress}>
      {children}
    </ScanProgressContext.Provider>
  );
}

export function useScanProgressContext() {
  const context = useContext(ScanProgressContext);
  if (!context) {
    throw new Error('useScanProgressContext must be used within ScanProgressProvider');
  }
  return context;
}
