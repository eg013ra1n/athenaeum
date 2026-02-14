import { openUrl } from '@tauri-apps/plugin-opener';
import { ExternalLink } from 'lucide-react';

const frontendLibs = [
  'React',
  'React Router',
  'Tauri API',
  'TanStack Table',
  'Tailwind CSS',
  'Lucide React',
  'date-fns',
  'Vite',
];

const backendLibs = [
  'Tauri 2.0',
  'rusqlite',
  'serde',
  'tokio',
  'rayon',
  'chrono',
  'rustafits',
  'xxhash-rust',
  'walkdir',
  'quick-xml',
  'anyhow',
];

function ExtLink({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <button
      onClick={() => openUrl(href)}
      className="inline-flex items-center gap-1 text-accent hover:text-accent-hover transition-colors"
    >
      {children}
      <ExternalLink size={14} />
    </button>
  );
}

export default function About() {
  return (
    <div className="p-6 max-w-3xl mx-auto space-y-8">
      <div>
        <h1 className="text-3xl font-medium text-success font-antiqua tracking-wide">ATHENAEUM</h1>
        <p className="text-content-muted text-sm mt-1">
          v{__APP_VERSION__} ({__GIT_COMMIT__})
        </p>
      </div>

      <section className="space-y-2">
        <h2 className="text-lg font-semibold text-content">About</h2>
        <p className="text-content-secondary leading-relaxed">
          Athenaeum is a desktop catalog for astrophotography image files. It extracts metadata
          from FITS and XISF files, automatically groups frames into sets by sky coordinates,
          manages calibration frame matching, and provides export tools for organizing your
          imaging library.
        </p>
      </section>

      <section className="space-y-2">
        <h2 className="text-lg font-semibold text-content">Creator</h2>
        <p className="text-content-secondary">
          Vilen Sharifov
        </p>
        <div className="flex gap-4">
          <ExtLink href="https://app.astrobin.com/u/sharifov">AstroBin</ExtLink>
          <ExtLink href="https://github.com/eg013ra1n">GitHub</ExtLink>
        </div>
      </section>

      <section className="space-y-3">
        <h2 className="text-lg font-semibold text-content">Libraries</h2>
        <div className="grid grid-cols-2 gap-6">
          <div>
            <h3 className="text-sm font-medium text-content-muted mb-2">Frontend</h3>
            <ul className="space-y-1">
              {frontendLibs.map((lib) => (
                <li key={lib} className="text-content-secondary text-sm">{lib}</li>
              ))}
            </ul>
          </div>
          <div>
            <h3 className="text-sm font-medium text-content-muted mb-2">Backend</h3>
            <ul className="space-y-1">
              {backendLibs.map((lib) => (
                <li key={lib} className="text-content-secondary text-sm">{lib}</li>
              ))}
            </ul>
          </div>
        </div>
      </section>
    </div>
  );
}
