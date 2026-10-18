# rewritten_package_json_must_be_as_left

`tsdown` with `exports: true` reads `package.json` for the version it bundles and writes the `exports` field back into the same file. The file as the build left it is its fingerprint: a second run hits. A version bump is a miss, the real build bundles the new version, and the bump survives.

## `vt run build`

cache miss: writes exports into package.json

```
$ tsdown
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/tsdown.config.ts 
ℹ entry: src/index.ts
ℹ Build start
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>
```

## `vtt grep-file package.json exports`

```
package.json: found "exports"
```

## `vtt grep-file dist/index.mjs 1.0.0`

```
dist/index.mjs: found "1.0.0"
```

## `vt run build`

cache hit: package.json is as the build left it

```
$ tsdown ◉ cache hit, replaying
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/tsdown.config.ts 
ℹ entry: src/index.ts
ℹ Build start
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>

---
vt run: cache hit.
```

## `vtt replace-file-content package.json '"version": "1.0.0"' '"version": "1.1.0"'`

bump the version

```
```

## `vt run build`

cache miss: package.json changed since the cached run

```
$ tsdown ○ cache miss: 'package.json' modified, executing
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/tsdown.config.ts 
ℹ entry: src/index.ts
ℹ Build start
ℹ Cleaning 1 files
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>
```

## `vtt grep-file dist/index.mjs 1.1.0`

```
dist/index.mjs: found "1.1.0"
```

## `vtt grep-file package.json 1.1.0`

the bump survives the build

```
package.json: found "1.1.0"
```

## `vt run build`

cache hit

```
$ tsdown ◉ cache hit, replaying
ℹ tsdown v0.23.1 powered by rolldown v1.2.14
ℹ config file: <workspace>/tsdown.config.ts 
ℹ entry: src/index.ts
ℹ Build start
ℹ Cleaning 1 files
ℹ dist/index.mjs  0.08 kB │ gzip: 0.09 kB
ℹ 1 files, total: 0.08 kB
✔ Build complete in <duration>

---
vt run: cache hit.
```
