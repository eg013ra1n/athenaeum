import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { FolderOpen } from 'lucide-react';
import { openPath, revealItemInDir, copyPaths } from '../api/desktop';
import { isMac, isTauri } from '../utils/platform';
import { containingFolders, distinctPaths } from '../utils/fileLocations';
import { useNotifications } from '../contexts/NotificationContext';

interface Props {
  paths?: string[];
  loadPaths?: () => Promise<string[]>;
  label?: string;
  compact?: boolean;
  directories?: boolean;
}

/**
 * One location chooser for catalog files, including unavailable source paths.
 * Load membership on demand; merely opening the chooser performs no OS action.
 */
export function FileLocationActions({
  paths,
  loadPaths,
  label = 'File locations',
  compact = false,
  directories = false,
}: Props) {
  const [items, setItems] = useState<string[] | null>(null);
  const [page, setPage] = useState(0);
  const [feedback, setFeedback] = useState('');
  const [busy, setBusy] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (items !== null) dialog.current?.showModal();
  }, [items]);
  const { notify } = useNotifications();
  const report = (error: unknown) => {
    console.error('[FileLocationActions]', error);
    setFeedback(`Action failed: ${String(error)}. You can select and copy the path text.`);
    notify({
      title: 'File location action failed',
      detail: `${String(error)}. The file or drive may be unavailable; you can still copy the displayed path.`,
      kind: 'files',
      tone: 'warning',
      hasErrors: true,
    });
  };
  const run = async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (error) {
      report(error);
    }
  };
  const show = async () => {
    setBusy(true);
    setFeedback('');
    setPage(0);
    try {
      setItems(distinctPaths(loadPaths ? await loadPaths() : (paths ?? [])));
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  };
  const copy = (values: string[]) =>
    run(async () => {
      await copyPaths(values);
      setFeedback(`Copied ${values.length} path${values.length === 1 ? '' : 's'}.`);
      notify({
        title: `Copied ${values.length} path${values.length === 1 ? '' : 's'}`,
        detail: 'Paths copied to the clipboard.',
        kind: 'files',
        tone: 'success',
      });
    });
  const folders = directories ? (items ?? []) : containingFolders(items ?? []);
  return (
    <>
      <button
        type="button"
        disabled={busy}
        title={label}
        aria-label={label}
        onMouseDown={event => event.stopPropagation()}
        onClick={event => {
          event.stopPropagation();
          void show();
        }}
        className="inline-flex items-center gap-1 p-1 rounded text-content-muted hover:text-accent hover:bg-surface-hover disabled:opacity-50"
      >
        <FolderOpen size={14} />
        {!compact && (busy ? 'Loading locations…' : label)}
      </button>
      {items !== null &&
        createPortal(
          <dialog
            ref={dialog}
            aria-label={label}
            onClose={() => setItems(null)}
            onClick={event => event.stopPropagation()}
            onKeyDown={event => event.stopPropagation()}
            className="bg-surface text-content border border-border rounded-lg p-4 w-[640px] max-w-[90vw] max-h-[80vh] overflow-auto backdrop:bg-overlay/50"
          >
            <div className="flex justify-between items-center mb-3">
              <h2 className="font-semibold">{label}</h2>
              <button
                type="button"
                onClick={() => dialog.current?.close()}
                className="p-1 hover:text-accent"
              >
                Close
              </button>
            </div>
            <p className="text-xs text-content-muted mb-3">
              {isTauri
                ? 'Catalog paths may be missing or on an offline drive. Reveal errors leave paths available to copy.'
                : 'These are paths on the server. This browser cannot open its folders. Copy a path, or select the text if clipboard access is unavailable.'}
            </p>
            {feedback && (
              <p role="status" className="text-sm text-content-secondary mb-3">
                {feedback}
              </p>
            )}
            {!items?.length ? (
              <p>No file paths available.</p>
            ) : (
              <>
                <div className="flex gap-3 text-sm mb-3">
                  <button onClick={() => void copy(items)}>Copy all paths ({items.length})</button>
                  <button onClick={() => void copy(folders)}>
                    Copy folder paths ({folders.length})
                  </button>
                </div>
                <h3 className="text-sm font-semibold">
                  {directories ? 'Folders' : 'Containing folders'} ({folders.length})
                </h3>
                {folders.map(folder => (
                  <div key={folder} className="border-b border-border py-2 text-xs">
                    <span className="select-text break-all">{folder}</span>
                    <div className="flex gap-3 mt-1">
                      {isTauri && (
                        <button onClick={() => void run(() => openPath(folder))}>
                          Open folder
                        </button>
                      )}
                      <button onClick={() => void copy([folder])}>Copy folder path</button>
                    </div>
                  </div>
                ))}
                {!directories && (
                  <h3 className="text-sm font-semibold mt-4">Files ({items.length})</h3>
                )}
                {!directories &&
                  items.slice(page * 100, (page + 1) * 100).map(path => (
                    <div key={path} className="border-b border-border py-2 text-xs">
                      <span className="select-text break-all">{path}</span>
                      <div className="flex gap-3 mt-1">
                        {isTauri && (
                          <button onClick={() => void run(() => revealItemInDir(path))}>
                            {isMac ? 'Reveal in Finder' : 'Show in Folder'}
                          </button>
                        )}
                        <button onClick={() => void copy([path])}>Copy path</button>
                      </div>
                    </div>
                  ))}
                {!directories && items.length > 100 && (
                  <div className="flex gap-3 text-sm mt-3">
                    <button disabled={page === 0} onClick={() => setPage(page - 1)}>
                      Previous files
                    </button>
                    <span>
                      Page {page + 1} of {Math.ceil(items.length / 100)}
                    </span>
                    <button
                      disabled={(page + 1) * 100 >= items.length}
                      onClick={() => setPage(page + 1)}
                    >
                      Next files
                    </button>
                  </div>
                )}
              </>
            )}
          </dialog>,
          document.body,
        )}
    </>
  );
}
