// Export… and Import… of the FITS keyword rules (plan Task 8, spec §5.7). Desktop: native save/open dialogs and the
// core reads and writes the file by path. Web: a browser download and a hidden file input, the text goes to the server.
// Import never writes anything itself: it asks the server for a plan, shows the changed fields, and on Import hands
// the planned document to the section, whose autosave stores it.

import { useRef, useState } from 'react';
import { api } from '../../../api';
import { pickFile, pickSaveFile } from '../../../api/desktop';
import { useNotifications } from '../../../contexts/NotificationContext';
import { HEADER_FIELD_LABELS } from '../../../settings/headerFields';
import type { HeaderFallbackImportPlan } from '../../../types/models';
import { downloadTextFile, readFileText } from '../../../utils/browserFile';
import { isTauri } from '../../../utils/platform';
import { ConfirmDialog } from '../../ConfirmDialog';
import { Button } from '../../ui';

const FILE_NAME = 'athenaeum-fits-keywords.json';
const JSON_FILTERS = [{ name: 'JSON', extensions: ['json'] }];
// The server's own limit for an import file (`TooLarge`): a larger file is refused before it is read into memory.
const MAX_IMPORT_BYTES = 1_048_576;
const NO_CHANGE = 'This file matches your current keywords.';
const CATALOG_NOTE = 'Catalog values change only when you apply.';

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function confirmMessage(plan: HeaderFallbackImportPlan): string {
  if (plan.changes.length === 0) return NO_CHANGE;
  const lines = plan.changes.map(
    (c) => `${HEADER_FIELD_LABELS[c.field]}: ${c.before.join(', ')} → ${c.after.join(', ')}`,
  );
  lines.push(CATALOG_NOTE);
  return lines.join('\n');
}

interface HeaderKeywordsTransferProps {
  /** The plan the user confirmed (never called for a plan with no change). The receiver applies only `plan.changes`'
   *  fields onto the document as it is by then: the plan was computed earlier, and an edit made meanwhile stays. */
  onImport(plan: HeaderFallbackImportPlan): void;
}

export function HeaderKeywordsTransfer({ onImport }: HeaderKeywordsTransferProps) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState(false);
  const [plan, setPlan] = useState<HeaderFallbackImportPlan | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const exportRules = async () => {
    setBusy(true);
    try {
      if (isTauri) {
        const path = await pickSaveFile({ defaultPath: FILE_NAME, filters: JSON_FILTERS });
        if (!path) return;
        await api.invoke('export_header_fallbacks', { path });
      } else {
        const text = await api.invoke<string>('export_header_fallbacks', {});
        downloadTextFile(FILE_NAME, text);
      }
    } catch (err) {
      console.error('[HeaderKeywords] export failed:', err);
      notify({
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
        title: 'FITS keywords not exported',
        detail: errMsg(err),
      });
    } finally {
      setBusy(false);
    }
  };

  const planImport = async (args: { path: string } | { text: string }) => {
    setBusy(true);
    try {
      setPlan(await api.invoke<HeaderFallbackImportPlan>('plan_header_fallbacks_import', args));
    } catch (err) {
      console.error('[HeaderKeywords] import refused:', err);
      notify({
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
        title: 'FITS keywords not imported',
        detail: errMsg(err),
      });
    } finally {
      setBusy(false);
    }
  };

  const importRules = async () => {
    if (!isTauri) {
      fileInput.current?.click();
      return;
    }
    let path: string | null;
    try {
      path = await pickFile({ title: 'Import FITS keywords', filters: JSON_FILTERS });
    } catch (err) {
      console.error('[HeaderKeywords] file dialog failed:', err);
      notify({
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
        title: 'FITS keywords not imported',
        detail: errMsg(err),
      });
      return;
    }
    if (path) await planImport({ path });
  };

  const onFileChosen = async (input: HTMLInputElement) => {
    const file = input.files?.[0];
    // Cleared so choosing the same file again fires another change.
    input.value = '';
    if (!file) return;
    if (file.size > MAX_IMPORT_BYTES) {
      console.error('[HeaderKeywords] import file too large:', file.size);
      notify({
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
        title: 'FITS keywords not imported',
        detail: 'the file is larger than 1 MB',
      });
      return;
    }
    let text: string;
    try {
      text = await readFileText(file);
    } catch (err) {
      console.error('[HeaderKeywords] file read failed:', err);
      notify({
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
        title: 'FITS keywords not imported',
        detail: errMsg(err),
      });
      return;
    }
    await planImport({ text });
  };

  const confirm = () => {
    const chosen = plan;
    setPlan(null);
    if (chosen && chosen.changes.length > 0) onImport(chosen);
  };

  return (
    <>
      <Button size="sm" disabled={busy} onClick={() => void exportRules()}>
        Export…
      </Button>
      <Button size="sm" disabled={busy} onClick={() => void importRules()}>
        Import…
      </Button>
      {!isTauri && (
        <input
          ref={fileInput}
          type="file"
          accept=".json,application/json"
          className="hidden"
          aria-label="FITS keywords file"
          data-testid="header-keywords-file"
          onChange={(e) => void onFileChosen(e.currentTarget)}
        />
      )}
      <ConfirmDialog
        isOpen={plan !== null}
        title="Import FITS keywords?"
        message={plan ? confirmMessage(plan) : ''}
        confirmText="Import"
        onConfirm={confirm}
        onCancel={() => setPlan(null)}
      />
    </>
  );
}
