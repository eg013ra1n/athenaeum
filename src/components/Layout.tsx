import { useState, useEffect } from 'react';
import { Outlet, NavLink } from 'react-router-dom';
import { Files, Calendar, Target, Focus, Camera, Layers, Settings, Trash2, Info, ChevronsLeft, ChevronsRight } from 'lucide-react';
import { ScanProgressProvider } from '../contexts/ScanProgressContext';
import { ScanProgressIndicator } from './ScanProgressIndicator';
import { ExportProgressProvider } from '../contexts/ExportProgressContext';
import { ExportProgressIndicator } from './ExportProgressIndicator';
import { AnalysisProgressProvider } from '../contexts/AnalysisProgressContext';
import { AnalysisQueueIndicator } from './AnalysisQueueIndicator';
import Logo from '../assets/athenaeum.png';

export default function Layout() {
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem('sidebar-collapsed') === 'true'
  );

  useEffect(() => {
    localStorage.setItem('sidebar-collapsed', String(collapsed));
  }, [collapsed]);

  const navItems = [
    { to: '/files', icon: Files, label: 'File Manager' },
    { to: '/objects', icon: Target, label: 'Objects' },
    { to: '/equipment', icon: Camera, label: 'Equipment' },
    { to: '/skychart', icon: Focus, label: 'Sky Chart' },
    { to: '/calendar', icon: Calendar, label: 'Shoot Calendar' },
    { to: '/export', icon: Layers, label: 'Export' },
    { to: '/blackhole', icon: Trash2, label: 'Black Hole' },
    { to: '/settings', icon: Settings, label: 'Settings' },
    { to: '/about', icon: Info, label: 'About' },
  ];

  return (
    <ScanProgressProvider>
      <ExportProgressProvider>
        <AnalysisProgressProvider>
        <div className="flex h-screen bg-surface text-content">
          {/* Sidebar Navigation */}
          <aside
            className={`${collapsed ? 'w-16' : 'w-64'} bg-surface-elevated border-r border-border transition-all duration-200 overflow-hidden flex flex-col shrink-0`}
          >
            <div className={`p-4 border-b border-border flex items-center ${collapsed ? 'justify-center' : 'gap-3'}`}>
              <img src={Logo} alt="Athenaeum" className="w-12 h-auto shrink-0" />
              {!collapsed && (
                <div>
                  <h1 className="text-2xl font-medium text-success font-antiqua tracking-wide">ATHENAEUM</h1>
                  <p className="text-xs text-content-muted">Astrophotography Library</p>
                </div>
              )}
            </div>

            <nav className={`${collapsed ? 'p-2' : 'p-4'} space-y-2 flex-1`}>
              {navItems.map(({ to, icon: Icon, label }) => (
                <NavLink
                  key={to}
                  to={to}
                  title={collapsed ? label : undefined}
                  className={({ isActive }) =>
                    `flex items-center ${collapsed ? 'justify-center px-0' : 'gap-3 px-4'} py-3 rounded-lg transition-colors ${
                      isActive
                        ? 'bg-accent text-surface'
                        : 'text-content-secondary hover:bg-surface-hover'
                    }`
                  }
                >
                  <Icon size={20} className="shrink-0" />
                  {!collapsed && <span>{label}</span>}
                </NavLink>
              ))}
            </nav>

            <AnalysisQueueIndicator collapsed={collapsed} />

            <div className={`${collapsed ? 'p-2' : 'p-4'} pt-0`}>
              <button
                onClick={() => setCollapsed(c => !c)}
                aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
                title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
                className={`flex items-center ${collapsed ? 'justify-center px-0' : 'gap-3 px-4'} py-3 rounded-lg transition-colors text-content-muted hover:bg-surface-hover w-full`}
              >
                {collapsed ? <ChevronsRight size={20} /> : <ChevronsLeft size={20} />}
                {!collapsed && <span>Collapse</span>}
              </button>
            </div>
          </aside>

          {/* Main Content */}
          <main className="flex-1 overflow-auto">
            <Outlet />
          </main>

          {/* Global progress indicators */}
          <ScanProgressIndicator />
          <ExportProgressIndicator />
        </div>
        </AnalysisProgressProvider>
      </ExportProgressProvider>
    </ScanProgressProvider>
  );
}
