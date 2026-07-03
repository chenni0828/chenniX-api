import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/admin/api': 'http://localhost:8081',
      '/v1': 'http://localhost:8081',
    },
  },
  build: {
    outDir: '../static',
    emptyOutDir: true,
  },
})
