/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
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
          muted: 'rgba(163, 190, 140, 0.25)', // nord14 @ 25% - backgrounds
        },
        warning: {
          DEFAULT: '#ebcb8b', // nord13 - warning text/icons
          muted: 'rgba(235, 203, 139, 0.25)', // nord13 @ 25% - backgrounds
        },
        error: {
          DEFAULT: '#bf616a', // nord11 - error text/icons
          muted: 'rgba(191, 97, 106, 0.25)', // nord11 @ 25% - backgrounds
        },
        info: {
          DEFAULT: '#81a1c1', // nord9 - info text/icons
          muted: 'rgba(129, 161, 193, 0.25)', // nord9 @ 25% - backgrounds
        },
        // Additional Aurora colors
        orange: {
          DEFAULT: '#d08770', // nord12 - orange accent
        },
        purple: {
          DEFAULT: '#b48ead', // nord15 - purple accent
        },
      }
    },
  },
  plugins: [],
}
