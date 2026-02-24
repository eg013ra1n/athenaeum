import { useState } from 'react';
import { api } from '../api';
import { openUrl } from '../api/desktop';
import { isTauri } from '../utils/platform';
import { RefreshCw, Download, CheckCircle2, AlertCircle, Info, ExternalLink } from 'lucide-react';

interface UpdateInfo {
  current_version: string;
  latest_version: string;
  is_update_available: boolean;
  download_url: string;
}

interface Dependency {
  name: string;
  url: string;
  license: string;
  copyright: string;
}

const frontendDeps: Dependency[] = [
  { name: 'React', url: 'https://github.com/facebook/react', license: 'MIT', copyright: 'Meta Platforms, Inc.' },
  { name: 'React Router', url: 'https://github.com/remix-run/react-router', license: 'MIT', copyright: 'Remix Software Inc.' },
  { name: 'TanStack Table', url: 'https://github.com/TanStack/table', license: 'MIT', copyright: 'Tanner Linsley' },
  { name: 'Tauri API', url: 'https://github.com/tauri-apps/tauri', license: 'MIT OR Apache-2.0', copyright: 'Tauri Programme within The Commons Conservancy' },
  { name: 'clsx', url: 'https://github.com/lukeed/clsx', license: 'MIT', copyright: 'Luke Edwards' },
  { name: 'date-fns', url: 'https://github.com/date-fns/date-fns', license: 'MIT', copyright: 'Sasha Koss' },
  { name: 'Lucide React', url: 'https://github.com/lucide-icons/lucide', license: 'ISC', copyright: 'Lucide Contributors' },
  { name: 'D3.js', url: 'https://github.com/d3/d3', license: 'BSD-3-Clause', copyright: 'Mike Bostock' },
  { name: 'd3-geo-projection', url: 'https://github.com/d3/d3-geo-projection', license: 'BSD-3-Clause', copyright: 'Mike Bostock' },
  { name: 'd3-celestial', url: 'https://github.com/ofrohn/d3-celestial', license: 'BSD-3-Clause', copyright: 'Olaf Frohn' },
];

const backendDeps: Dependency[] = [
  { name: 'Tauri', url: 'https://github.com/tauri-apps/tauri', license: 'MIT OR Apache-2.0', copyright: 'Tauri Programme within The Commons Conservancy' },
  { name: 'rusqlite', url: 'https://github.com/rusqlite/rusqlite', license: 'MIT', copyright: 'rusqlite contributors' },
  { name: 'serde', url: 'https://github.com/serde-rs/serde', license: 'MIT OR Apache-2.0', copyright: 'David Tolnay' },
  { name: 'tokio', url: 'https://github.com/tokio-rs/tokio', license: 'MIT', copyright: 'Tokio Contributors' },
  { name: 'rayon', url: 'https://github.com/rayon-rs/rayon', license: 'MIT OR Apache-2.0', copyright: 'Niko Matsakis, Josh Stone' },
  { name: 'chrono', url: 'https://github.com/chronotope/chrono', license: 'MIT OR Apache-2.0', copyright: 'Kang Seonghoon' },
  { name: 'rustafits', url: 'https://github.com/eg013ra1n/rustafits', license: 'Apache-2.0', copyright: 'Vilen Sharifov' },
  { name: 'xxhash-rust', url: 'https://github.com/DoumanAsh/xxhash-rust', license: 'BSL-1.0', copyright: 'Douman' },
  { name: 'walkdir', url: 'https://github.com/BurntSushi/walkdir', license: 'Unlicense OR MIT', copyright: 'Andrew Gallant' },
  { name: 'quick-xml', url: 'https://github.com/tafia/quick-xml', license: 'MIT', copyright: 'Johann Tuffe' },
  { name: 'anyhow', url: 'https://github.com/dtolnay/anyhow', license: 'MIT OR Apache-2.0', copyright: 'David Tolnay' },
  { name: 'base64', url: 'https://github.com/marshallpierce/rust-base64', license: 'MIT OR Apache-2.0', copyright: 'Marshall Pierce' },
];

function UpdateSection() {
  const [checking, setChecking] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);

  const handleCheck = async () => {
    setChecking(true);
    setCheckError(null);
    setUpdateInfo(null);
    try {
      const info = await api.invoke<UpdateInfo>('check_for_updates');
      setUpdateInfo(info);
    } catch (err) {
      const msg = typeof err === 'string' ? err : 'Failed to check for updates';
      console.error('check_for_updates:', err);
      setCheckError(msg);
    } finally {
      setChecking(false);
    }
  };

  return (
    <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-4">
      <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">Updates</h2>
      <button
        onClick={handleCheck}
        disabled={checking}
        className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed text-sm"
      >
        <RefreshCw size={15} className={checking ? 'animate-spin' : ''} />
        {checking ? 'Checking…' : 'Check for Updates'}
      </button>

      {checkError && (
        <div className="flex items-start gap-2 p-3 bg-error/10 border border-error/40 rounded-lg text-sm text-error">
          <AlertCircle size={15} className="flex-shrink-0 mt-0.5" />
          {checkError}
        </div>
      )}

      {updateInfo && !updateInfo.is_update_available && (
        <div className="flex items-center gap-2 p-3 bg-success/10 border border-success/40 rounded-lg text-sm text-success">
          <CheckCircle2 size={15} />
          You're up to date! (v{updateInfo.current_version})
        </div>
      )}

      {updateInfo && updateInfo.is_update_available && (
        <div className="p-3 bg-accent/10 border border-accent/40 rounded-lg space-y-2">
          <div className="flex items-center gap-2 text-sm font-semibold text-accent">
            <Info size={15} />
            Version {updateInfo.latest_version} is available
          </div>
          <p className="text-sm text-content-muted">
            You are running v{updateInfo.current_version}.
          </p>
          <button
            onClick={() => openUrl(updateInfo.download_url)}
            className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover rounded-lg transition text-sm"
          >
            <Download size={15} />
            Download v{updateInfo.latest_version}
            <ExternalLink size={13} />
          </button>
        </div>
      )}
    </section>
  );
}

function ExtLink({ href, children }: { href: string; children: React.ReactNode }) {
  const handleClick = () => {
    if (isTauri) {
      openUrl(href);
    } else {
      window.open(href, '_blank', 'noopener');
    }
  };

  return (
    <button
      onClick={handleClick}
      className="inline-flex items-center gap-1 text-accent hover:text-accent-hover transition-colors"
    >
      {children}
      <ExternalLink size={14} />
    </button>
  );
}

function DependencyTable({ deps }: { deps: Dependency[] }) {
  return (
    <table className="w-full text-sm">
      <thead>
        <tr className="text-left text-content-muted border-b border-border">
          <th className="pb-2 font-medium">Library</th>
          <th className="pb-2 font-medium">License</th>
          <th className="pb-2 font-medium">Copyright</th>
        </tr>
      </thead>
      <tbody className="divide-y divide-border/50">
        {deps.map((dep) => (
          <tr key={dep.name}>
            <td className="py-1.5">
              <ExtLink href={dep.url}>{dep.name}</ExtLink>
            </td>
            <td className="py-1.5 text-content-secondary font-mono text-xs">{dep.license}</td>
            <td className="py-1.5 text-content-secondary">{dep.copyright}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export default function About() {
  return (
    <div className="p-8 max-w-4xl mx-auto space-y-10">
      <div className="text-center py-6">
        <h1 className="text-4xl font-medium text-success font-antiqua tracking-widest">ATHENAEUM</h1>
        <p className="text-content-muted text-sm mt-2 font-mono">
          v{__APP_VERSION__} ({__GIT_COMMIT__})
        </p>
      </div>

      <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-3">
        <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">About</h2>
        <p className="text-content-secondary leading-relaxed">
          Athenaeum is a desktop catalog for astrophotography image files. It extracts metadata
          from FITS and XISF files, automatically groups frames into sets by sky coordinates,
          manages calibration frame matching and provides export tools for organizing your
          imaging library.
        </p>
      </section>

      {isTauri && <UpdateSection />}

      <div className="grid grid-cols-3 gap-6">
        <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-3">
          <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">Creator</h2>
          <p className="text-content-secondary">
            Vilen Sharifov
          </p>
          <div className="flex gap-4">
            <ExtLink href="https://app.astrobin.com/u/sharifov">AstroBin</ExtLink>
            <ExtLink href="https://github.com/eg013ra1n">GitHub</ExtLink>
          </div>
        </section>

        <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-3">
          <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">License</h2>
          <p className="text-content-secondary leading-relaxed">
            Athenaeum is licensed under the{' '}
            <ExtLink href="https://www.apache.org/licenses/LICENSE-2.0">
              Apache License 2.0
            </ExtLink>
          </p>
        </section>

        <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-3">
          <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">Support</h2>
          <p className="text-content-secondary leading-relaxed">
            If you find Athenaeum useful, consider supporting its development.
          </p>
          <button
            onClick={() => isTauri ? openUrl('https://ko-fi.com/N4N81UR2EE') : window.open('https://ko-fi.com/N4N81UR2EE', '_blank', 'noopener')}
            className="hover:opacity-80 transition-opacity"
          >
            <img
              src="https://ko-fi.com/img/githubbutton_sm.svg"
              alt="Support on Ko-fi"
              height="30"
            />
          </button>
        </section>
      </div>

      <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-5">
        <div>
          <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">Third-Party Notices</h2>
          <p className="text-content-muted text-sm mt-1">
            Athenaeum is built with the following open-source libraries.
          </p>
        </div>

        <div>
          <div>
            <h3 className="text-xs font-semibold uppercase tracking-wider text-content-muted mb-3">Frontend</h3>
            <DependencyTable deps={frontendDeps} />
          </div>
          <div className="mt-10">
            <h3 className="text-xs font-semibold uppercase tracking-wider text-content-muted mb-3">Backend</h3>
            <DependencyTable deps={backendDeps} />
          </div>
        </div>
      </section>
    </div>
  );
}
