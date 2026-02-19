import { createContext, useContext, ReactNode } from 'react';
import { useExportProgress } from '../hooks/useExportProgress';

type ExportProgressContextType = ReturnType<typeof useExportProgress>;

const ExportProgressContext = createContext<ExportProgressContextType | null>(null);

export function ExportProgressProvider({ children }: { children: ReactNode }) {
  const exportProgress = useExportProgress();

  return (
    <ExportProgressContext.Provider value={exportProgress}>
      {children}
    </ExportProgressContext.Provider>
  );
}

export function useExportProgressContext() {
  const context = useContext(ExportProgressContext);
  if (!context) {
    throw new Error('useExportProgressContext must be used within ExportProgressProvider');
  }
  return context;
}
