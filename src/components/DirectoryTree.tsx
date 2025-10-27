import { useState, useMemo } from 'react';
import { ChevronRight, ChevronDown, Folder, File as FileIcon } from 'lucide-react';
import type { FileWithFrame } from '../types/models';

interface DirectoryNode {
  name: string;
  path: string;
  isDirectory: boolean;
  children: Map<string, DirectoryNode>;
  files: FileWithFrame[];
}

interface DirectoryTreeProps {
  files: FileWithFrame[];
}

function buildTree(files: FileWithFrame[]): DirectoryNode {
  // Determine common base path of all files to use as tree root
  const commonPrefix = (() => {
    if (files.length === 0) return '';
    const splitPaths = files.map(f => f.file.path.split('/').filter(Boolean));
    let prefix: string[] = [];
    for (let i = 0; ; i++) {
      const segment = splitPaths[0][i];
      if (!segment) break;
      if (splitPaths.every(p => p[i] === segment)) {
        prefix.push(segment);
      } else {
        break;
      }
    }
    return prefix.join('/');
  })();

  const root: DirectoryNode = {
    name: commonPrefix || 'root',
    path: commonPrefix,
    isDirectory: true,
    children: new Map(),
    files: [],
  };

  files.forEach((fileWithFrame) => {
    const parts = fileWithFrame.file.path.split('/');
    let currentNode = root;

    // Navigate/create directories
    for (let i = 0; i < parts.length - 1; i++) {
      const dirName = parts[i];
      if (!dirName) continue; // Skip empty parts (from leading /)

      if (!currentNode.children.has(dirName)) {
        const dirPath = parts.slice(0, i + 1).join('/');
        currentNode.children.set(dirName, {
          name: dirName,
          path: dirPath,
          isDirectory: true,
          children: new Map(),
          files: [],
        });
      }
      currentNode = currentNode.children.get(dirName)!;
    }

    // Add file to the final directory
    currentNode.files.push(fileWithFrame);
  });

  return root;
}

interface TreeNodeProps {
  node: DirectoryNode;
  level: number;
}

function collectFiles(node: DirectoryNode): FileWithFrame[] {
  let all: FileWithFrame[] = [...node.files];
  node.children.forEach(child => {
    all = all.concat(collectFiles(child));
  });
  return all;
}

function TreeNode({ node, level }: TreeNodeProps) {
  const [expanded, setExpanded] = useState(level === 0);

  const totalFiles = useMemo(() => {
    const countFiles = (n: DirectoryNode): number => {
      let count = n.files.length;
      n.children.forEach((child) => {
        count += countFiles(child);
      });
      return count;
    };
    return countFiles(node);
  }, [node]);

  if (level === 0) {
    // Root node - just render children
    return (
      <div className="space-y-1">
        {Array.from(node.children.values()).map((child) => (
          <TreeNode key={child.path} node={child} level={level + 1} />
        ))}
      </div>
    );
  }

  return (
    <div>
      {/* Directory Header */}
      <div
        className="flex items-center gap-2 py-2 px-3 hover:bg-gray-700 rounded cursor-pointer transition group"
        style={{ paddingLeft: `${level * 1.5}rem` }}
        onClick={() => setExpanded(!expanded)}
      >
        <div className="flex-shrink-0">
          {expanded ? (
            <ChevronDown size={16} className="text-gray-400" />
          ) : (
            <ChevronRight size={16} className="text-gray-400" />
          )}
        </div>
        <Folder size={16} className="text-blue-400 flex-shrink-0" />
        <span className="font-medium text-sm flex-1 truncate">{node.name}</span>
        <span className="text-xs text-gray-500 flex-shrink-0">
          {totalFiles} {totalFiles === 1 ? 'file' : 'files'}
        </span>
      </div>

      {/* Expanded Content */}
      {expanded && (
        <div className="space-y-1">
          {/* Child Directories */}
          {Array.from(node.children.values()).map((child) => (
            <TreeNode key={child.path} node={child} level={level + 1} />
          ))}

          {/* Files in this directory */}
          {collectFiles(node).map((fileWithFrame, idx) => (
            <div
              key={fileWithFrame.file.id || idx}
              className="flex items-center gap-2 py-2 px-3 hover:bg-gray-750 rounded transition group"
              style={{ paddingLeft: `${(level + 1) * 1.5}rem` }}
            >
              <FileIcon size={14} className="text-gray-500 flex-shrink-0" />
              <div className="flex-1 min-w-0 grid grid-cols-7 gap-4 items-center text-sm">
                <span className="font-mono text-xs truncate col-span-2" title={fileWithFrame.file.path}>
                  {fileWithFrame.file.path.split('/').pop()}
                </span>
                <span className="truncate text-gray-300" title={fileWithFrame.frame?.object}>
                  {fileWithFrame.frame?.object || '-'}
                </span>
                <span className="truncate text-gray-400 text-xs">
                  {fileWithFrame.frame?.telescop || '-'}
                </span>
                <span className="truncate text-gray-400 text-xs">
                  {fileWithFrame.frame?.filter || '-'}
                </span>
                <span className="text-right text-gray-400 text-xs">
                  {fileWithFrame.frame?.exptime ? `${fileWithFrame.frame.exptime}s` : '-'}
                </span>
                <span className="text-right text-gray-500 text-xs">
                  {(fileWithFrame.file.size / 1024 / 1024).toFixed(1)} MB
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export default function DirectoryTree({ files }: DirectoryTreeProps) {
  const tree = useMemo(() => buildTree(files), [files]);

  return (
    <div className="bg-gray-800 rounded-lg p-4">
      <div className="mb-3 px-3">
        <div className="grid grid-cols-7 gap-4 text-xs font-semibold text-gray-400 uppercase">
          <span className="col-span-2">Filename</span>
          <span>Object</span>
          <span>Telescope</span>
          <span>Filter</span>
          <span className="text-right">Exposure</span>
          <span className="text-right">Size</span>
        </div>
      </div>
      <div className="border-t border-gray-700 pt-2">
        <TreeNode node={tree} level={0} />
      </div>
    </div>
  );
}
