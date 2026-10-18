# emptied_dist_may_be_missing

A rebuild on a tree that has `dist` empties it and fills it again: that run did not create `dist`, but `dist` holds nothing except the build's outputs. Its cache entry accepts a tree without `dist` too and restores it, like an entry recorded on a clean checkout.

## `vt run build`

cache miss, creates dist/

```
$ tsdown
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/package.json 
ℹ entry: src/index.ts
ℹ Build start
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>
```

## `vtt replace-file-content src/index.ts v1 v2`

a source change

```
```

## `vt run build`

cache miss: tsdown empties dist/ and fills it again

```
$ tsdown ○ cache miss: 'src/index.ts' modified, executing
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/package.json 
ℹ entry: src/index.ts
ℹ Build start
ℹ Cleaning 1 files
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>
```

## `vtt rm -rf dist`

a clean checkout of this commit has no dist/

```
```

## `vt run build`

cache hit: dist/ held only the build's outputs, so it may be missing

```
$ tsdown ◉ cache hit, replaying
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/package.json 
ℹ entry: src/index.ts
ℹ Build start
ℹ Cleaning 1 files
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>

---
vt run: cache hit.
```

## `vtt stat-file dist/index.mjs`

restored from the cache

```
dist/index.mjs: exists
```
