import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// Pietro frontend — see pietro.md §14.1.
// Two plugins, one proxy block. If anything else lands here, justify it.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    // Local Rust backend listens on :18080 (see pietro.yaml).
    // /api and /proxy are the only two prefixes the SPA talks to.
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:18080',
        changeOrigin: false,
      },
      '/proxy': {
        target: 'http://127.0.0.1:18080',
        changeOrigin: false,
      },
    },
  },
})
