import { createContext, useContext, ReactNode } from 'react';
import { useLightCalibration } from '../hooks/useLightCalibration';

type LightCalibrationContextType = ReturnType<typeof useLightCalibration>;

const LightCalibrationContext = createContext<LightCalibrationContextType | null>(null);

export function LightCalibrationProvider({ children }: { children: ReactNode }) {
  const value = useLightCalibration();
  return (
    <LightCalibrationContext.Provider value={value}>
      {children}
    </LightCalibrationContext.Provider>
  );
}

export function useLightCalibrationContext() {
  const ctx = useContext(LightCalibrationContext);
  if (!ctx) {
    throw new Error('useLightCalibrationContext must be used within LightCalibrationProvider');
  }
  return ctx;
}
