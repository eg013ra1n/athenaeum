import React from 'react';
import { Button, DialogShell } from './ui';

interface ConfirmDialogProps {
  isOpen: boolean;
  title: string;
  message: string;
  onConfirm: () => void;
  onCancel: () => void;
  confirmText?: string;
  cancelText?: string;
  confirmDanger?: boolean;
}

export const ConfirmDialog: React.FC<ConfirmDialogProps> = ({
  isOpen, title, message, onConfirm, onCancel, confirmText = 'Confirm', cancelText = 'Cancel', confirmDanger = false,
}) => {
  if (!isOpen) return null;
  return (
    <DialogShell
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel} data-autofocus={confirmDanger ? true : undefined}>{cancelText}</Button>
          <Button variant={confirmDanger ? 'dangerPrimary' : 'primary'} onClick={onConfirm} data-autofocus={confirmDanger ? undefined : true}>{confirmText}</Button>
        </>
      }
    >
      <p className="whitespace-pre-line">{message}</p>
    </DialogShell>
  );
};
