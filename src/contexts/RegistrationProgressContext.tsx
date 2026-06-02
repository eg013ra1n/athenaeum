import { createContext, useContext, ReactNode } from 'react';
import { useRegistrationProgress } from '../hooks/useRegistrationProgress';

type RegistrationProgressContextType = ReturnType<typeof useRegistrationProgress>;

const RegistrationProgressContext = createContext<RegistrationProgressContextType | null>(null);

export function RegistrationProgressProvider({ children }: { children: ReactNode }) {
  const value = useRegistrationProgress();
  return (
    <RegistrationProgressContext.Provider value={value}>
      {children}
    </RegistrationProgressContext.Provider>
  );
}

export function useRegistrationProgressContext() {
  const ctx = useContext(RegistrationProgressContext);
  if (!ctx) {
    throw new Error(
      'useRegistrationProgressContext must be used within RegistrationProgressProvider',
    );
  }
  return ctx;
}
