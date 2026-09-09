/** Catalog spelling is preserved: do not canonicalize paths or collapse siblings. */
export function containingFolder(path: string): string {
  const windows = /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\');
  const separator = windows && path.includes('\\') ? '\\' : '/';
  const index = path.lastIndexOf(separator);
  if (index < 0) return '.';
  if (index === 0) return separator;
  if (windows && index === 2 && path[1] === ':') return path.slice(0, 3);
  return path.slice(0, index);
}

export function distinctPaths(paths: string[]): string[] {
  return [...new Set(paths.filter(path => path.length > 0))];
}

export function containingFolders(paths: string[]): string[] {
  return distinctPaths(paths.map(containingFolder));
}
