import React from 'react';
import { X, AlertCircle, AlertTriangle, Info } from 'lucide-react';

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

  const variantStyles = {
    error: {
      border: 'border-error',
      icon: <AlertCircle className="w-6 h-6 text-error" />,
      iconBg: 'bg-error-muted',
    },
    warning: {
      border: 'border-warning',
      icon: <AlertTriangle className="w-6 h-6 text-warning" />,
      iconBg: 'bg-warning-muted',
    },
    info: {
      border: 'border-info',
      icon: <Info className="w-6 h-6 text-info" />,
      iconBg: 'bg-info-muted',
    },
  };

  const style = variantStyles[variant];

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className={`bg-surface-elevated rounded-lg p-6 max-w-md w-full mx-4 border ${style.border} shadow-xl`}>
        <div className="flex items-start gap-4">
          <div className={`${style.iconBg} rounded-full p-2 flex-shrink-0`}>
            {style.icon}
          </div>
          <div className="flex-1">
            <h3 className="text-lg font-semibold text-content mb-2">{title}</h3>
            <p className="text-content-secondary whitespace-pre-line">{message}</p>
          </div>
          {showCloseButton && (
            <button
              onClick={onClose}
              className="text-content-muted hover:text-content transition-colors flex-shrink-0"
            >
              <X className="w-5 h-5" />
            </button>
          )}
        </div>
        <div className="flex justify-end mt-6">
          <button
            onClick={onClose}
            className="px-4 py-2 bg-surface-hover text-content rounded hover:brightness-110 transition-colors"
          >
            OK
          </button>
        </div>
      </div>
    </div>
  );
};
