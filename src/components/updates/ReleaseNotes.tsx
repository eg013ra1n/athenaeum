// Release-notes Markdown → token-styled elements. Links open outside the
// webview (opener / window.open), never as in-app navigation.

import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { openUrl } from '../../api/desktop';

export function ReleaseNotes({ markdown }: { markdown: string }) {
  return (
    <div className="text-sm text-content-secondary leading-relaxed">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => <h1 className="text-base font-semibold text-content mt-4 mb-2">{children}</h1>,
          h2: ({ children }) => <h2 className="text-sm font-semibold uppercase tracking-wider text-accent mt-5 mb-2">{children}</h2>,
          h3: ({ children }) => <h3 className="text-sm font-semibold text-content mt-3 mb-1">{children}</h3>,
          p: ({ children }) => <p className="my-2">{children}</p>,
          em: ({ children }) => <em className="text-content-muted">{children}</em>,
          strong: ({ children }) => <strong className="text-content font-semibold">{children}</strong>,
          ul: ({ children }) => <ul className="list-disc pl-5 my-2 space-y-1">{children}</ul>,
          ol: ({ children }) => <ol className="list-decimal pl-5 my-2 space-y-1">{children}</ol>,
          li: ({ children }) => <li>{children}</li>,
          code: ({ children }) => <code className="rounded bg-surface px-1 py-0.5 font-mono text-xs text-content">{children}</code>,
          pre: ({ children }) => <pre className="rounded bg-surface p-3 my-2 overflow-x-auto font-mono text-xs">{children}</pre>,
          table: ({ children }) => <table className="my-2 w-full text-xs border-collapse">{children}</table>,
          th: ({ children }) => <th className="border border-border px-2 py-1 text-left text-content">{children}</th>,
          td: ({ children }) => <td className="border border-border px-2 py-1">{children}</td>,
          a: ({ href, children }) => (
            <a
              href={href}
              className="text-accent underline hover:text-accent-hover"
              onClick={(e) => {
                e.preventDefault();
                if (href) void openUrl(href);
              }}
            >
              {children}
            </a>
          ),
        }}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}
