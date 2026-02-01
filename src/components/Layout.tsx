import { Outlet, NavLink } from 'react-router-dom';
import { Files, Calendar, Target, Map, Camera, Layers, Settings, Trash2 } from 'lucide-react';
import { ScanProgressProvider } from '../contexts/ScanProgressContext';
import { ScanProgressIndicator } from './ScanProgressIndicator';
import Logo from '../assets/athenaeum.svg';

export default function Layout() {
  const navItems = [
    { to: '/files', icon: Files, label: 'File Manager' },
    { to: '/calendar', icon: Calendar, label: 'Shoot Calendar' },
    { to: '/objects', icon: Target, label: 'Objects' },
    { to: '/skyatlas', icon: Map, label: 'Sky Atlas' },
    { to: '/equipment', icon: Camera, label: 'Equipment' },
    { to: '/export', icon: Layers, label: 'Export' },
    { to: '/blackhole', icon: Trash2, label: 'Black Hole' },
    { to: '/settings', icon: Settings, label: 'Settings' },
  ];

  return (
    <ScanProgressProvider>
      <div className="flex h-screen bg-surface text-content">
        {/* Sidebar Navigation */}
        <aside className="w-64 bg-surface-elevated border-r border-border">
          <div className="p-4 border-b border-border flex items-center gap-3">
            <img src={Logo} alt="Athenaeum" className="w-12" />
            <div>
              <h1 className="text-2xl font-medium text-success font-antiqua tracking-wide">ATHENAEUM</h1>
              <p className="text-xs text-content-muted">Astrophotography Library</p>
            </div>
          </div>

          <nav className="p-4 space-y-2">
            {navItems.map(({ to, icon: Icon, label }) => (
              <NavLink
                key={to}
                to={to}
                className={({ isActive }) =>
                  `flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ${
                    isActive
                      ? 'bg-accent text-surface'
                      : 'text-content-secondary hover:bg-surface-hover'
                  }`
                }
              >
                <Icon size={20} />
                <span>{label}</span>
              </NavLink>
            ))}
          </nav>
        </aside>

        {/* Main Content */}
        <main className="flex-1 overflow-auto">
          <Outlet />
        </main>

        {/* Global scan progress indicator */}
        <ScanProgressIndicator />
      </div>
    </ScanProgressProvider>
  );
}
