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
      border: 'border-red-600',
      icon: <AlertCircle className="w-6 h-6 text-red-500" />,
      iconBg: 'bg-red-900/30',
    },
    warning: {
      border: 'border-yellow-600',
      icon: <AlertTriangle className="w-6 h-6 text-yellow-500" />,
      iconBg: 'bg-yellow-900/30',
    },
    info: {
      border: 'border-blue-600',
      icon: <Info className="w-6 h-6 text-blue-500" />,
      iconBg: 'bg-blue-900/30',
    },
  };

  const style = variantStyles[variant];

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className={`bg-gray-800 rounded-lg p-6 max-w-md w-full mx-4 border ${style.border} shadow-xl`}>
        <div className="flex items-start gap-4">
          <div className={`${style.iconBg} rounded-full p-2 flex-shrink-0`}>
            {style.icon}
          </div>
          <div className="flex-1">
            <h3 className="text-lg font-semibold text-gray-100 mb-2">{title}</h3>
            <p className="text-gray-300 whitespace-pre-line">{message}</p>
          </div>
          {showCloseButton && (
            <button
              onClick={onClose}
              className="text-gray-400 hover:text-gray-200 transition-colors flex-shrink-0"
            >
              <X className="w-5 h-5" />
            </button>
          )}
        </div>
        <div className="flex justify-end mt-6">
          <button
            onClick={onClose}
            className="px-4 py-2 bg-gray-700 text-gray-200 rounded hover:bg-gray-600 transition-colors"
          >
            OK
          </button>
        </div>
      </div>
    </div>
  );
};
