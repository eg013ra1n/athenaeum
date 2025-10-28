import { Outlet, NavLink } from 'react-router-dom';
import { Files, Calendar, Target, Camera, Upload, Settings } from 'lucide-react';

export default function Layout() {
  const navItems = [
    { to: '/files', icon: Files, label: 'File Manager' },
    { to: '/calendar', icon: Calendar, label: 'Shoot Calendar' },
    { to: '/objects', icon: Target, label: 'Objects' },
    { to: '/equipment', icon: Camera, label: 'Equipment' },
    { to: '/export', icon: Upload, label: 'Export' },
    { to: '/settings', icon: Settings, label: 'Settings' },
  ];

  return (
    <div className="flex h-screen bg-gray-900 text-gray-100">
      {/* Sidebar Navigation */}
      <aside className="w-64 bg-gray-800 border-r border-gray-700">
        <div className="p-4 border-b border-gray-700">
          <h1 className="text-2xl font-bold text-blue-400">Athenaeum</h1>
          <p className="text-xs text-gray-400 mt-1">Astrophotography Manager</p>
        </div>

        <nav className="p-4 space-y-2">
          {navItems.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                `flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ${
                  isActive
                    ? 'bg-blue-600 text-white'
                    : 'text-gray-300 hover:bg-gray-700'
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
    </div>
  );
}
