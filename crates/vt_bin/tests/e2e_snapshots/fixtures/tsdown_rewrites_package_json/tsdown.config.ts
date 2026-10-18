import { defineConfig } from 'tsdown';

export default defineConfig({
  entry: ['src/*.ts'],
  // Writes the `exports` field into package.json.
  exports: true,
});
