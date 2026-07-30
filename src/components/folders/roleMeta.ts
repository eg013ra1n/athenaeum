import { Folder, Library, Inbox, Users, Archive, type LucideIcon } from 'lucide-react';

export type RoleKind = 'calibration_library' | 'sync_incoming' | 'collaboration';
export type AddableKind = 'normal' | 'archive' | RoleKind;

export type RailSelection =
  | { type: 'scan'; id: number }
  | { type: 'archive'; id: number }
  | { type: 'placeholder'; kind: RoleKind };

export interface RoleMeta {
  kind: RoleKind;
  label: string;
  icon: LucideIcon;
  /** Icon tint (design token text class) — role-colored lucide, decision D5. */
  tint: string;
  /** Badge chip classes (bg/text/border tokens). */
  chip: string;
  /** One-liner for placeholder rows and the Add dialog. */
  purpose: string;
  /** Placement rule shown in the Add dialog BEFORE picking (spec §6). */
  placementRule: string;
  /** Inspector explainer card text (spec §5.2). */
  explainer: string;
  /** Switch visibility matrix (spec §5.2). */
  switches: { watch: boolean; duplicates: boolean; uniqueCamera: boolean };
  getCommand: string;
  setCommand: string;
  clearCommand: string;
}

export const ROLE_ORDER: RoleKind[] = ['calibration_library', 'sync_incoming', 'collaboration'];

export const ROLE_META: Record<RoleKind, RoleMeta> = {
  calibration_library: {
    kind: 'calibration_library',
    label: 'Calibration Library',
    icon: Library,
    tint: 'text-purple',
    chip: 'bg-purple/20 text-purple border border-purple/40',
    purpose: 'Master calibration frames live here.',
    placementRule:
      'May be inside a monitored folder, or standalone — a standalone folder is also scanned, so masters you drop in by hand are imported.',
    explainer:
      'Master calibration frames built by Athenaeum are written here, and masters you drop in by hand are imported on scan.',
    switches: { watch: true, duplicates: false, uniqueCamera: false },
    getCommand: 'get_calibration_library_dir',
    setCommand: 'set_calibration_library_dir',
    clearCommand: 'clear_calibration_library_dir',
  },
  sync_incoming: {
    kind: 'sync_incoming',
    label: 'Sync Incoming',
    icon: Inbox,
    tint: 'text-accent',
    chip: 'bg-accent/20 text-accent border border-accent/40',
    purpose: 'Files received from your capture devices land here.',
    placementRule: 'Must be its own folder, outside every monitored folder.',
    explainer: 'Transfers from your paired capture devices land here and are cataloged on scan.',
    switches: { watch: true, duplicates: true, uniqueCamera: false },
    getCommand: 'get_sync_incoming_dir',
    setCommand: 'set_sync_incoming_dir',
    clearCommand: 'clear_sync_incoming_dir',
  },
  collaboration: {
    kind: 'collaboration',
    label: 'Collaboration',
    icon: Users,
    tint: 'text-success',
    chip: 'bg-success/20 text-success border border-success/40',
    purpose: 'Received project contributions are stored here.',
    placementRule: 'Must be its own folder, outside every monitored folder.',
    explainer: 'Contributions received for collaboration projects are stored here and cataloged on scan.',
    switches: { watch: true, duplicates: true, uniqueCamera: false },
    getCommand: 'get_collaboration_dir',
    setCommand: 'set_collaboration_dir',
    clearCommand: 'clear_collaboration_dir',
  },
};

export const KIND_META = {
  normal: {
    label: 'Monitored folder',
    icon: Folder,
    tint: 'text-info',
    purpose: 'Watch a folder of FITS/XISF files and catalog everything in it.',
    placementRule: "A monitored folder can't sit inside another monitored folder — pick a separate directory.",
  },
  archive: {
    label: 'Archive destination',
    icon: Archive,
    tint: 'text-warning',
    purpose: 'Where "Move and ZIP" stores finished sets. Not scanned.',
    placementRule: 'Never scanned — it may live anywhere, even inside a monitored folder.',
  },
} as const;

export function metaForKind(kind: AddableKind) {
  if (kind === 'normal' || kind === 'archive') return KIND_META[kind];
  return ROLE_META[kind];
}

/** Map a FolderCandidateVerdict to dialog copy (spec §6). */
export function verdictMessage(reason: string | null, conflictingPath: string | null): string {
  switch (reason) {
    case 'not_found':
      return 'This directory does not exist.';
    case 'not_a_directory':
      return 'This path is not a directory.';
    case 'already_monitored':
      return 'This folder is already monitored.';
    case 'inside_existing':
      return `This folder is inside «${conflictingPath ?? 'a monitored folder'}», which is already monitored. Choose a folder outside it.`;
    case 'contains_existing':
      return `This folder contains the monitored folder «${conflictingPath ?? ''}». Choose a folder that does not wrap an existing one.`;
    case 'role_taken':
      return `This role is already assigned to «${conflictingPath ?? ''}». Release it first, or use Change folder on that row.`;
    default:
      return 'This folder cannot be used here.';
  }
}
