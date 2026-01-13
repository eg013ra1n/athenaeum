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
    <div className="border border-gray-700 rounded-lg overflow-hidden">
      {/* Group Header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between p-3 bg-gray-800 hover:bg-gray-750 transition-colors"
      >
        <div className="flex items-center gap-2">
          {expanded ? (
            <ChevronDown size={16} className="text-gray-400" />
          ) : (
            <ChevronRight size={16} className="text-gray-400" />
          )}
          <Sun size={16} className="text-yellow-500" />
          <span className="font-medium">{group.name}</span>
        </div>
        <div className="flex items-center gap-4 text-sm text-gray-400">
          <span>{group.lightCount} frames</span>
          <span>{formatExposure(group.totalExposure)}</span>
          {group.subgroupCount > 1 && (
            <span className="px-2 py-0.5 bg-blue-900/50 text-blue-400 rounded text-xs">
              {group.subgroupCount} subgroups
            </span>
          )}
        </div>
      </button>

      {/* Tree Content */}
      {expanded && (
        <div className="p-3 bg-gray-900/50">
          {/* Stacking Summary */}
          <div className="mb-3 pb-3 border-b border-gray-700">
            <div className="text-sm text-gray-400">
              <span className="font-medium text-gray-300">Stacking:</span>{' '}
              {group.lightCount} light frames ({formatExposure(group.totalExposure)} total)
              {group.subgroupCount > 1 && (
                <span className="text-blue-400"> → {group.subgroupCount} separate stacks by exposure time</span>
              )}
              {group.subgroupCount === 1 && (
                <span className="text-green-400"> → 1 combined stack</span>
              )}
            </div>
          </div>

          {/* Calibration Hierarchy */}
          <div className="text-xs text-gray-500 mb-2 uppercase tracking-wide">Calibration Chain</div>
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
          hasChildren ? 'cursor-pointer hover:bg-gray-800/50 rounded' : ''
        }`}
        onClick={() => hasChildren && setExpanded(!expanded)}
      >
        {/* Expand/Collapse or spacer */}
        {hasChildren ? (
          expanded ? (
            <ChevronDown size={14} className="text-gray-500" />
          ) : (
            <ChevronRight size={14} className="text-gray-500" />
          )
        ) : (
          <span className="w-[14px]" />
        )}

        {/* Node Icon */}
        <Icon size={14} className={iconColor} />

        {/* Label with frame count */}
        <span
          className={`text-sm ${
            node.isMissing ? 'text-red-400 italic' : 'text-gray-200'
          }`}
        >
          {node.label}
          {node.count > 0 && node.nodeType !== 'Light' && (
            <span className="text-gray-500 ml-1">({node.count} frames)</span>
          )}
        </span>

        {/* Status indicators */}
        <div className="flex items-center gap-2 ml-auto">
          {node.isShared && (
            <span title="Shared with other groups">
              <Share2 size={12} className="text-blue-400" />
            </span>
          )}
          {node.warnings.length > 0 && (
            <span title={node.warnings.join('\n')}>
              <AlertTriangle size={12} className="text-yellow-500" />
            </span>
          )}
          {!node.isMissing && node.nodeType !== 'Light' && (
            <Check size={12} className="text-green-500" />
          )}
          {node.isMissing && <X size={12} className="text-red-500" />}
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
  if (isMissing) return 'text-red-500';

  switch (nodeType) {
    case 'Light':
      return 'text-yellow-500';
    case 'Flat':
      return 'text-blue-400';
    case 'Dark':
      return 'text-purple-400';
    case 'Bias':
      return 'text-gray-400';
    case 'DarkFlat':
      return 'text-indigo-400';
    default:
      return 'text-gray-400';
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
