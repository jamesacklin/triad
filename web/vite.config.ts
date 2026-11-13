import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { '@': path.resolve(import.meta.dirname, './src') },
  },
  build: {
    // One chunk by design (no routes to split on). Markdown (react-markdown
    // and remark-gfm) took it past Vite's 500 kB warning; FINDINGS.md "Web
    // app" records the size. Raise this only with a reason.
    chunkSizeWarningLimit: 600,
  },
  test: {
    environment: 'node',
  },
})
