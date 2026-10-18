import fs from 'node:fs';

// Build into a temporary folder and rename it into place, as tools that
// replace their output folder at once do.
fs.rmSync('dist', { recursive: true, force: true });
fs.mkdirSync('dist.tmp', { recursive: true });
fs.writeFileSync('dist.tmp/a.js', 'built\n');
fs.renameSync('dist.tmp', 'dist');
