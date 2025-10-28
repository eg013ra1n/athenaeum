export default function WelcomeScreen() {
  return (
    <div className="min-h-screen bg-gray-900 flex items-center justify-center">
      <div className="text-center">
        <h1 className="text-5xl font-bold text-gray-100 mb-4">Athenaeum</h1>
        <p className="text-gray-400 mb-8">Astrophotography Image Management</p>

        <div className="flex items-center justify-center gap-3 text-gray-400">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-500"></div>
          <span>Initializing database...</span>
        </div>
      </div>
    </div>
  );
}
