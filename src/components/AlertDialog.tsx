import React from 'react';
import { AlertCircle, AlertTriangle, Info } from 'lucide-react';
import { Button, DialogShell } from './ui';

interface AlertDialogProps {
  isOpen: boolean;
  title: string;
  message: string;
  onClose: () => void;
  variant?: 'error' | 'warning' | 'info';
  showCloseButton?: boolean;
}

export const AlertDialog: React.FC<AlertDialogProps> = ({
  isOpen,
  title,
  message,
  onClose,
  variant = 'info',
  showCloseButton = true,
}) => {
  if (!isOpen) return null;

  const icon =
    variant === 'error' ? <AlertCircle size={14} className="inline shrink-0 text-error" aria-hidden />
    : variant === 'warning' ? <AlertTriangle size={14} className="inline shrink-0 text-warning" aria-hidden />
    : <Info size={14} className="inline shrink-0 text-info" aria-hidden />;

  return (
    <DialogShell
      title={
        <>
          {icon} {title}
        </>
      }
      onClose={onClose}
      footer={
        showCloseButton !== false ? (
          <Button variant="primary" onClick={onClose} data-autofocus>OK</Button>
        ) : undefined
      }
    >
      <p className="whitespace-pre-line">{message}</p>
    </DialogShell>
  );
};
