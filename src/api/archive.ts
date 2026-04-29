import { api } from './index';
import type {
  ArchiveCompression,
  ArchivedFrameSetSummary,
  ArchiveOperationSummary,
  ArchivePlan,
  ArchiveSettings,
  ConflictResolution,
  Dispositions,
} from '../types/archive';

export async function getArchiveSettings(): Promise<ArchiveSettings> {
  return api.invoke<ArchiveSettings>('get_archive_settings');
}

export async function setArchiveRootPath(path: string): Promise<void> {
  await api.invoke('set_archive_root_path', { path });
}

export async function setArchiveCompression(compression: ArchiveCompression): Promise<void> {
  await api.invoke('set_archive_compression', { compression });
}

export async function planArchiveOperation(
  framesSetId: number,
  dispositions: Dispositions,
  compression: ArchiveCompression,
): Promise<ArchivePlan> {
  return api.invoke<ArchivePlan>('plan_archive_operation', {
    framesSetId,
    dispositions,
    compression,
  });
}

export async function startArchiveOperation(
  framesSetId: number,
  dispositions: Dispositions,
  compression: ArchiveCompression,
  conflictResolution: ConflictResolution,
): Promise<number> {
  return api.invoke<number>('start_archive_operation', {
    framesSetId,
    dispositions,
    compression,
    conflictResolution,
  });
}

export async function cancelArchiveOperation(operationId: number): Promise<void> {
  await api.invoke('cancel_archive_operation', { operationId });
}

export async function listUnfinishedArchiveOperations(): Promise<ArchiveOperationSummary[]> {
  return api.invoke<ArchiveOperationSummary[]>('list_unfinished_archive_operations');
}

export async function resumeArchiveOperation(operationId: number): Promise<void> {
  await api.invoke('resume_archive_operation', { operationId });
}

export async function rollbackArchiveOperation(operationId: number): Promise<void> {
  await api.invoke('rollback_archive_operation', { operationId });
}

export async function listArchivedFrameSets(): Promise<ArchivedFrameSetSummary[]> {
  return api.invoke<ArchivedFrameSetSummary[]>('list_archived_frame_sets');
}

export async function startRestoreOperation(
  operationId: number,
  targetRootPath: string,
  overwriteExisting: boolean,
  keepZipAfterRestore: boolean,
): Promise<void> {
  await api.invoke('start_restore_operation', {
    operationId,
    targetRootPath,
    overwriteExisting,
    keepZipAfterRestore,
  });
}

export async function deleteArchive(operationId: number): Promise<void> {
  await api.invoke('delete_archive', { operationId });
}
