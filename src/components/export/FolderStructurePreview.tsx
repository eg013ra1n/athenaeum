import { useState } from 'react';
import { ChevronDown, ChevronRight, Folder, File, MoreHorizontal, HardDrive } from 'lucide-react';
import type { FolderPreview, FolderNode, FolderNodeType } from '../../types/export';

interface FolderStructurePreviewProps {
  preview: FolderPreview;
  estimatedSizeBytes: number;
}

/**
 * Interactive folder structure preview showing the export layout
 */
export function FolderStructurePreview({
  preview,
  estimatedSizeBytes,
}: FolderStructurePreviewProps) {
  return (
    <div className="border border-border rounded-lg overflow-hidden bg-surface">
      {/* Header */}
      <div className="p-3 bg-surface-elevated border-b border-border">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-medium text-content-muted uppercase tracking-wide">
            Export Folder Structure
          </h3>
          <div className="flex items-center gap-4 text-sm text-content-muted">
            <div className="flex items-center gap-1.5">
              <File size={14} />
              <span>
                {preview.totalFiles} files
              </span>
            </div>
            <div className="flex items-center gap-1.5">
              <HardDrive size={14} />
              <span>{formatBytes(estimatedSizeBytes)} (estimated)</span>
            </div>
          </div>
        </div>
      </div>

      {/* Tree View */}
      <div className="p-3 font-mono text-sm">
        {/* Root folder */}
        <div className="flex items-center gap-2 py-1">
          <Folder size={16} className="text-warning" />
          <span className="text-content">{preview.rootName}/</span>
        </div>

        {/* Children */}
        <div className="ml-4">
          {preview.structure.map((node, index) => (
            <FolderTreeNode key={index} node={node} depth={0} />
          ))}
        </div>
      </div>
    </div>
  );
}

interface FolderTreeNodeProps {
  node: FolderNode;
  depth: number;
}

function FolderTreeNode({ node, depth }: FolderTreeNodeProps) {
  const [expanded, setExpanded] = useState(depth < 2);
  const hasChildren = node.children.length > 0;
  const isFolder = node.nodeType === 'folder';
  const isEllipsis = node.nodeType === 'ellipsis';

  return (
    <div style={{ marginLeft: depth * 16 }}>
      {/* Node row */}
      <div
        className={`flex items-center gap-2 py-1 ${
          isFolder && hasChildren ? 'cursor-pointer hover:bg-surface-elevated/50 rounded -ml-1 pl-1' : ''
        }`}
        onClick={() => isFolder && hasChildren && setExpanded(!expanded)}
      >
        {/* Expand icon for folders with children */}
        {isFolder && hasChildren ? (
          expanded ? (
            <ChevronDown size={14} className="text-content-muted" />
          ) : (
            <ChevronRight size={14} className="text-content-muted" />
          )
        ) : (
          <span className="w-[14px]" />
        )}

        {/* Node icon */}
        <NodeIcon type={node.nodeType} />

        {/* Name */}
        <span className={`${isEllipsis ? 'text-content-muted italic' : 'text-content'}`}>
          {node.name}
          {isFolder && '/'}
        </span>

        {/* Description */}
        {node.description && (
          <span className="text-content-muted text-xs ml-2">{node.description}</span>
        )}
      </div>

      {/* Children */}
      {expanded && hasChildren && (
        <div>
          {node.children.map((child, index) => (
            <FolderTreeNode key={index} node={child} depth={depth + 1} />
          ))}
        </div>
      )}
    </div>
  );
}

function NodeIcon({ type }: { type: FolderNodeType }) {
  switch (type) {
    case 'folder':
      return <Folder size={14} className="text-warning" />;
    case 'file':
      return <File size={14} className="text-content-muted" />;
    case 'ellipsis':
      return <MoreHorizontal size={14} className="text-content-muted" />;
    default:
      return <File size={14} className="text-content-muted" />;
  }
}

/**
 * Format bytes to human-readable string
 */
function formatBytes(bytes: number): string {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  const TB = GB * 1024;

  if (bytes >= TB) {
    return `${(bytes / TB).toFixed(1)} TB`;
  } else if (bytes >= GB) {
    return `${(bytes / GB).toFixed(1)} GB`;
  } else if (bytes >= MB) {
    return `${(bytes / MB).toFixed(1)} MB`;
  } else if (bytes >= KB) {
    return `${(bytes / KB).toFixed(1)} KB`;
  } else {
    return `${bytes} B`;
  }
}
