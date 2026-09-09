import { useState, useEffect, useRef } from 'react';
import { Outlet, NavLink, useLocation } from 'react-router-dom';
import { Files, Calendar, Target, Focus, Camera, Settings, Trash2, Info, Users, ChevronsLeft, ChevronsRight, ArrowLeftRight } from 'lucide-react';
import { ArchiveResumeBanner } from './archive/ArchiveResumeBanner';
import { ScanProgressProvider } from '../contexts/ScanProgressContext';
import { ScanProgressIndicator } from './ScanProgressIndicator';
import { ExportProgressProvider } from '../contexts/ExportProgressContext';
import { ExportProgressIndicator } from './ExportProgressIndicator';
import { AnalysisProgressProvider } from '../contexts/AnalysisProgressContext';
import { AnalysisQueueIndicator } from './AnalysisQueueIndicator';
import { MasterBuildProvider } from '../contexts/MasterBuildContext';
import { StackingProvider } from '../contexts/StackingContext';
import { ComputeQueueIndicator } from './ComputeQueueIndicator';
import { PlateSolveProgressProvider } from '../contexts/PlateSolveProgressContext';
import { PlateSolveQueueIndicator } from './PlateSolveQueueIndicator';
import { RegistrationProgressProvider } from '../contexts/RegistrationProgressContext';
import { RegistrationQueueIndicator } from './RegistrationQueueIndicator';
import { PlateSolveIndexMissingModal } from './plate-solve';
import { NotificationProvider } from '../contexts/NotificationContext';
import { NavHistoryProvider, useNavHistory } from '../contexts/NavHistoryContext';
import { SessionStateProvider } from '../contexts/SessionStateContext';
import { useGlobalNavKeys } from '../hooks/useGlobalNavKeys';
import { useScrollRestoration } from '../hooks/useScrollRestoration';
import { NotificationBell } from './NotificationBell';
import { ToastStack } from './Toast';
import { NotificationPanel } from './NotificationPanel';
import { TransfersProvider } from '../contexts/TransfersContext';
import { TransferIndicator } from './transfers/TransferIndicator';
import { TransfersPanel } from './transfers/TransfersPanel';
import { AutoUpdateCheck } from './AutoUpdateCheck';
import { useProjectMatches } from '../hooks/useProjectMatches';
import { useContentIndexNotifications } from '../hooks/useContentIndex';
import Logo from '../assets/athenaeum.png';

/** Mounts the global `project-set-match` listener. Rendered inside
 * `NotificationProvider` (below) so `useProjectMatches` → `useNotifications`
 * has its context; the Layout body itself runs above the provider. */
function ProjectMatchesListener() {
  useProjectMatches();
  return null;
}

/** Mounts the global `content-index-finished` listener, for the same reason
 * `ProjectMatchesListener` exists: the hook needs `useNotifications`, which is
 * only available below `NotificationProvider`. */
function ContentIndexListener() {
  useContentIndexNotifications();
  return null;
}

/** Mounts the app-wide back/forward shortcuts. Rendered inside
 * `NavHistoryProvider` for the same reason the listeners above live in their own
 * components: the hook needs a context the Layout body itself sits above. */
function GlobalNavKeys() {
  const { back, forward } = useNavHistory();
  useGlobalNavKeys(back, forward);
  return null;
}

export default function Layout() {
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem('sidebar-collapsed') === 'true'
  );
  const { pathname } = useLocation();
  const contentRef = useRef<HTMLDivElement>(null);
  useScrollRestoration(contentRef);

  useEffect(() => {
    localStorage.setItem('sidebar-collapsed', String(collapsed));
  }, [collapsed]);

  const navItems = [
    { to: '/files', icon: Files, label: 'File Manager' },
    { to: '/objects', icon: Target, label: 'Objects' },
    { to: '/projects', icon: Users, label: 'Projects' },
    { to: '/equipment', icon: Camera, label: 'Equipment' },
    { to: '/skychart', icon: Focus, label: 'Sky Chart' },
    { to: '/calendar', icon: Calendar, label: 'Shoot Calendar' },
    { to: '/blackhole', icon: Trash2, label: 'Black Hole' },
    { to: '/transfers', icon: ArrowLeftRight, label: 'Transfers' },
    { to: '/settings', icon: Settings, label: 'Settings' },
    { to: '/about', icon: Info, label: 'About' },
  ];

  return (
    <NavHistoryProvider>
    <SessionStateProvider>
    <NotificationProvider>
    <TransfersProvider>
    <ScanProgressProvider>
      <ExportProgressProvider>
        <AnalysisProgressProvider>
        <PlateSolveProgressProvider>
        <RegistrationProgressProvider>
        <MasterBuildProvider>
        <StackingProvider>
        <div className="flex h-screen bg-surface text-content">
          {/* Sidebar Navigation */}
          <aside
            className={`${collapsed ? 'w-16' : 'w-64'} bg-surface-elevated border-r border-border transition-all duration-200 overflow-hidden flex flex-col shrink-0`}
          >
            <div className={`p-4 flex items-center ${collapsed ? 'justify-center' : 'gap-3'}`}>
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
                  // Clicking the page you are already on must not stack a
                  // duplicate history entry — Back would then appear to do
                  // nothing. Exact match only: /objects while on /objects/42 is
                  // a real navigation and still pushes.
                  replace={pathname === to}
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
            <ComputeQueueIndicator collapsed={collapsed} />
            <TransferIndicator collapsed={collapsed} />
            <PlateSolveQueueIndicator collapsed={collapsed} />
            <RegistrationQueueIndicator collapsed={collapsed} />
            <NotificationBell collapsed={collapsed} />

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
          <main className="flex-1 overflow-auto flex flex-col">
            <ArchiveResumeBanner />
            <div ref={contentRef} className="flex-1 overflow-auto">
              <Outlet />
            </div>
          </main>

          {/* Global progress indicators */}
          <ScanProgressIndicator />
          <ExportProgressIndicator />
          <PlateSolveIndexMissingModal />
          <ToastStack />
          <NotificationPanel />
          <TransfersPanel />
          <AutoUpdateCheck />
          <ProjectMatchesListener />
          <ContentIndexListener />
          <GlobalNavKeys />
        </div>
        </StackingProvider>
        </MasterBuildProvider>
        </RegistrationProgressProvider>
        </PlateSolveProgressProvider>
        </AnalysisProgressProvider>
      </ExportProgressProvider>
    </ScanProgressProvider>
    </TransfersProvider>
    </NotificationProvider>
    </SessionStateProvider>
    </NavHistoryProvider>
  );
}
