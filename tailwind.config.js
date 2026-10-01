/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      fontFamily: {
        antiqua: ['"Book Antiqua"', 'Palatino', '"Palatino Linotype"', 'serif'],
        mono: ['ui-monospace', '"SF Mono"', 'Menlo', 'Consolas', 'monospace'],
      },
      colors: {
        // Polar Night (backgrounds)
        surface: {
          DEFAULT: '#2e3440', // nord0 - base background
          elevated: '#3b4252', // nord1 - elevated surfaces
          hover: '#434c5e', // nord2 - hover states
        },
        // Snow Storm (text)
        content: {
          DEFAULT: '#eceff4', // nord6 - primary text
          secondary: '#e5e9f0', // nord5 - secondary text
          muted: '#d8dee9', // nord4 - muted text
          faint: 'rgba(216, 222, 233, 0.62)',
          ghost: 'rgba(216, 222, 233, 0.38)',
        },
        border: {
          DEFAULT: '#4c566a', // nord3 - borders
        },
        // Frost (primary accent)
        accent: {
          DEFAULT: '#88c0d0', // nord8 - primary buttons, links
          hover: '#81a1c1', // nord9 - hover state
          muted: '#5e81ac', // nord10 - subtle accents
        },
        // Aurora (semantic states)
        success: {
          DEFAULT: '#a3be8c', // nord14 - success text/icons
          muted: 'rgba(163, 190, 140, 0.22)', // nord14 @ 22% - backgrounds
        },
        warning: {
          DEFAULT: '#ebcb8b', // nord13 - warning text/icons
          muted: 'rgba(235, 203, 139, 0.22)', // nord13 @ 22% - backgrounds
        },
        error: {
          DEFAULT: '#bf616a', // nord11 - error text/icons
          muted: 'rgba(191, 97, 106, 0.22)', // nord11 @ 22% - backgrounds
        },
        info: {
          DEFAULT: '#81a1c1', // nord9 - info text/icons
          muted: 'rgba(129, 161, 193, 0.22)', // nord9 @ 22% - backgrounds
        },
        // Additional Aurora colors
        orange: {
          DEFAULT: '#d08770', // nord12 - orange accent
        },
        purple: {
          DEFAULT: '#b48ead', // nord15 - purple accent
        },
        line: {
          DEFAULT: 'rgba(76, 86, 106, 0.55)', // mockup --line — card borders, separators
          soft: 'rgba(76, 86, 106, 0.28)',    // frame-table row separator
          plain: 'rgba(76, 86, 106, 0.30)',   // plain-table row separator
        },
        table: {
          head: '#333a47',
          group: '#323946',
          'group-l1': '#353c4a',
          'group-hover': '#3a4251',
          'row-hover': 'rgba(67, 76, 94, 0.55)',   // frame-table row hover (spec §5.1)
          'plain-hover': 'rgba(67, 76, 94, 0.45)', // plain-table clickable row hover (§5.4)
          'peer-hover': 'rgba(67, 76, 94, 0.30)',  // Exchange peer row hover (§12)
        },
        teal: { DEFAULT: '#8fbcbb' }, // nord7
      }
    },
  },
  plugins: [],
}
