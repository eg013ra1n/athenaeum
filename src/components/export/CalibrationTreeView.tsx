import { useState } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Sun,
  Moon,
  Circle,
  AlertTriangle,
  Check,
  X,
  Share2,
} from 'lucide-react';
import type { CalibrationTreeNode, CalibrationRouteGroup } from '../../types/export';

interface CalibrationTreeViewProps {
  groups: CalibrationRouteGroup[];
}

export function CalibrationTreeView({ groups }: CalibrationTreeViewProps) {
  return (
    <div className="space-y-4">
      {groups.map((group, index) => (
        <GroupTree key={index} group={group} />
      ))}
    </div>
  );
}

interface GroupTreeProps {
  group: CalibrationRouteGroup;
}

function GroupTree({ group }: GroupTreeProps) {
  const [expanded, setExpanded] = useState(true);

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      {/* Group Header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between p-3 bg-surface-elevated hover:bg-surface-hover transition-colors"
      >
        <div className="flex items-center gap-2">
          {expanded ? (
            <ChevronDown size={16} className="text-content-muted" />
          ) : (
            <ChevronRight size={16} className="text-content-muted" />
          )}
          <Sun size={16} className="text-warning" />
          <span className="font-medium">{group.name}</span>
        </div>
        <div className="flex items-center gap-4 text-sm text-content-muted">
          <span>{group.lightCount} frames</span>
          <span>{formatExposure(group.totalExposure)}</span>
          {group.subgroupCount > 1 && (
            <span className="px-2 py-0.5 bg-info-muted text-accent rounded text-xs">
              {group.subgroupCount} subgroups
            </span>
          )}
        </div>
      </button>

      {/* Tree Content */}
      {expanded && (
        <div className="p-3 bg-surface/50">
          {/* Stacking Summary */}
          <div className="mb-3 pb-3 border-b border-border">
            <div className="text-sm text-content-muted">
              <span className="font-medium text-content-secondary">Stacking:</span>{' '}
              {group.lightCount} light frames ({formatExposure(group.totalExposure)} total)
              {group.subgroupCount > 1 && (
                <span className="text-accent"> → {group.subgroupCount} separate stacks by exposure time</span>
              )}
              {group.subgroupCount === 1 && (
                <span className="text-success"> → 1 combined stack</span>
              )}
            </div>
          </div>

          {/* Calibration Hierarchy */}
          <div className="text-xs text-content-muted mb-2 uppercase tracking-wide">Calibration Chain</div>
          {group.calibrationTree.map((node, index) => (
            <TreeNode key={index} node={node} depth={0} />
          ))}
        </div>
      )}
    </div>
  );
}

interface TreeNodeProps {
  node: CalibrationTreeNode;
  depth: number;
}

function TreeNode({ node, depth }: TreeNodeProps) {
  const [expanded, setExpanded] = useState(true);
  const hasChildren = node.children.length > 0;

  const Icon = getNodeIcon(node.nodeType);
  const iconColor = getIconColor(node.nodeType, node.isMissing);

  return (
    <div style={{ marginLeft: depth * 20 }}>
      <div
        className={`flex items-center gap-2 py-1.5 ${
          hasChildren ? 'cursor-pointer hover:bg-surface-elevated/50 rounded' : ''
        }`}
        onClick={() => hasChildren && setExpanded(!expanded)}
      >
        {/* Expand/Collapse or spacer */}
        {hasChildren ? (
          expanded ? (
            <ChevronDown size={14} className="text-content-muted" />
          ) : (
            <ChevronRight size={14} className="text-content-muted" />
          )
        ) : (
          <span className="w-[14px]" />
        )}

        {/* Node Icon */}
        <Icon size={14} className={iconColor} />

        {/* Label with frame count */}
        <span
          className={`text-sm ${
            node.isMissing ? 'text-error italic' : 'text-content'
          }`}
        >
          {node.label}
          {node.count > 0 && node.nodeType !== 'Light' && (
            <span className="text-content-muted ml-1">({node.count} frames)</span>
          )}
        </span>

        {/* Status indicators */}
        <div className="flex items-center gap-2 ml-auto">
          {node.isShared && (
            <span title="Shared with other groups">
              <Share2 size={12} className="text-accent" />
            </span>
          )}
          {node.warnings.length > 0 && (
            <span title={node.warnings.join('\n')}>
              <AlertTriangle size={12} className="text-warning" />
            </span>
          )}
          {!node.isMissing && node.nodeType !== 'Light' && (
            <Check size={12} className="text-success" />
          )}
          {node.isMissing && <X size={12} className="text-error" />}
        </div>
      </div>

      {/* Children */}
      {expanded &&
        hasChildren &&
        node.children.map((child, index) => (
          <TreeNode key={index} node={child} depth={depth + 1} />
        ))}
    </div>
  );
}

function getNodeIcon(nodeType: CalibrationTreeNode['nodeType']) {
  switch (nodeType) {
    case 'Light':
      return Sun;
    case 'Flat':
      return Circle;
    case 'Dark':
      return Moon;
    case 'Bias':
      return Circle;
    case 'DarkFlat':
      return Moon;
    default:
      return Circle;
  }
}

function getIconColor(nodeType: CalibrationTreeNode['nodeType'], isMissing: boolean): string {
  if (isMissing) return 'text-error';

  switch (nodeType) {
    case 'Light':
      return 'text-warning';
    case 'Flat':
      return 'text-accent';
    case 'Dark':
      return 'text-purple';
    case 'Bias':
      return 'text-content-muted';
    case 'DarkFlat':
      return 'text-indigo-400';
    default:
      return 'text-content-muted';
  }
}

function formatExposure(seconds: number): string {
  if (seconds < 60) {
    return `${seconds.toFixed(0)}s`;
  } else if (seconds < 3600) {
    const minutes = seconds / 60;
    return `${minutes.toFixed(1)}m`;
  } else {
    const hours = seconds / 3600;
    return `${hours.toFixed(2)}h`;
  }
}
