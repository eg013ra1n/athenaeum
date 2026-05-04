// Catalog search bar for the dual-pane file browser.
//
// Behavior:
//  - User types -> debounced query against `search_catalog` Tauri/web command.
//  - Results show as a dropdown listing path + key metadata.
//  - Click a hit -> calls onReveal(hit) so the parent can navigate the
//    active pane to the hit's parent directory and select the file.

import { useEffect, useRef, useState } from 'react';
import { Search, X } from 'lucide-react';
import { api } from '../../api';
import type { CatalogSearchHit } from './types';

interface CatalogSearchProps {
  onReveal: (hit: CatalogSearchHit) => void;
}

const DEBOUNCE_MS = 250;

export default function CatalogSearch({ onReveal }: CatalogSearchProps) {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<CatalogSearchHit[]>([]);
  const [loading, setLoading] = useState(false);
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  // Debounced query.
  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      setResults([]);
      setOpen(false);
      return;
    }
    setLoading(true);
    const timer = setTimeout(async () => {
      try {
        const hits = await api.invoke<CatalogSearchHit[]>('search_catalog', {
          query: trimmed,
          limit: 100,
        });
        setResults(hits);
        setOpen(true);
      } catch (e) {
        console.error('search_catalog failed:', e);
        setResults([]);
      } finally {
        setLoading(false);
      }
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  // Close on outside click.
  useEffect(() => {
    function onClickOutside(e: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener('mousedown', onClickOutside);
    return () => document.removeEventListener('mousedown', onClickOutside);
  }, []);

  return (
    <div ref={containerRef} className="relative">
      <div className="flex items-center gap-2 bg-surface-elevated border border-border rounded-md px-3 py-2">
        <Search size={16} className="text-content-muted" />
        <input
          ref={inputRef}
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onFocus={() => results.length > 0 && setOpen(true)}
          placeholder="Search catalog (filename, OBJECT, FILTER, INSTRUME, TELESCOP)…"
          className="flex-1 bg-transparent outline-none text-sm placeholder:text-content-muted"
        />
        {query && (
          <button
            onClick={() => setQuery('')}
            className="text-content-muted hover:text-content"
            title="Clear"
          >
            <X size={16} />
          </button>
        )}
        {loading && <span className="text-xs text-content-muted">…</span>}
      </div>

      {open && results.length > 0 && (
        <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-surface-elevated border border-border rounded-md shadow-lg max-h-96 overflow-y-auto">
          {results.map((hit) => (
            <button
              key={hit.fileId}
              onClick={() => {
                onReveal(hit);
                setOpen(false);
              }}
              className="w-full text-left px-3 py-2 hover:bg-surface border-b border-border last:border-0 text-sm"
            >
              <div className="font-mono text-content truncate">{hit.filename}</div>
              <div className="text-xs text-content-muted truncate">{hit.path}</div>
              <div className="flex gap-3 mt-0.5 text-xs text-content-muted">
                {hit.object && <span>OBJECT: {hit.object}</span>}
                {hit.filter && <span>FILTER: {hit.filter}</span>}
                {hit.imagetyp && <span>{hit.imagetyp}</span>}
                {hit.instrume && <span>{hit.instrume}</span>}
              </div>
            </button>
          ))}
        </div>
      )}

      {open && !loading && query.trim() && results.length === 0 && (
        <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-surface-elevated border border-border rounded-md shadow-lg p-3 text-sm text-content-muted">
          No matches.
        </div>
      )}
    </div>
  );
}
