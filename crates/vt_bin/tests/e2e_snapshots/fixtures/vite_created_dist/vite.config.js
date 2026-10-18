import { defineConfig } from 'vite';

export default defineConfig({
  logLevel: 'silent',
  build: {
    rollupOptions: {
      output: {
        // Stable filenames keep the snapshot independent of content hashes.
        entryFileNames: 'assets/main.js',
        chunkFileNames: 'assets/chunk.js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
});
