// Bundles src/index.ts to dist/example.js, keeping the plugin header (leading // lines) on top:
// esbuild drops comments, and Phoenix reads @name/@description/@version/@author/@api from there.
import { readFileSync } from 'node:fs';
import { build } from 'esbuild';

const header = /^(?:[ \t]*\/\/[^\n]*\n)*/.exec(readFileSync('src/index.ts', 'utf8'))[0].trimEnd();
await build({ entryPoints: ['src/index.ts'], bundle: true, format: 'iife', target: 'es2020', outfile: 'dist/example.js', banner: { js: header } });
