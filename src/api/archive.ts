import { api } from './index';
import type {
  ArchiveCompression,
  ArchivedFrameSetSummary,
  ArchiveOperationSummary,
  ArchivePlan,
  ArchiveRoot,
  ArchiveSettings,
  ArchiveZip,
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
  archiveRootPath?: string | null,
): Promise<ArchivePlan> {
  return api.invoke<ArchivePlan>('plan_archive_operation', {
    framesSetId,
    dispositions,
    compression,
    archiveRootPath: archiveRootPath ?? null,
  });
}

export async function startArchiveOperation(
  framesSetId: number,
  dispositions: Dispositions,
  compression: ArchiveCompression,
  conflictResolution: ConflictResolution,
  archiveRootPath?: string | null,
): Promise<number> {
  return api.invoke<number>('start_archive_operation', {
    framesSetId,
    dispositions,
    compression,
    conflictResolution,
    archiveRootPath: archiveRootPath ?? null,
  });
}

// Multi-folder archive root management

export async function listArchiveRoots(): Promise<ArchiveRoot[]> {
  return api.invoke<ArchiveRoot[]>('list_archive_roots');
}

export async function addArchiveRoot(path: string, label?: string | null): Promise<number> {
  return api.invoke<number>('add_archive_root', { path, label: label ?? null });
}

export async function deleteArchiveRoot(id: number): Promise<void> {
  await api.invoke('delete_archive_root', { id });
}

export async function setDefaultArchiveRoot(id: number): Promise<void> {
  await api.invoke('set_default_archive_root', { id });
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

export async function listArchiveZips(operationId: number): Promise<ArchiveZip[]> {
  return api.invoke<ArchiveZip[]>('list_archive_zips', { operationId });
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

export interface RestoreSuggestions {
  suggested_original_path: string | null;
  suggested_original_exists: boolean;
  scan_roots: string[];
}

export async function getRestoreSuggestions(operationId: number): Promise<RestoreSuggestions> {
  return api.invoke<RestoreSuggestions>('get_restore_suggestions', { operationId });
}
