# created_dist_may_be_missing_or_incomplete_but_not_stale

`vite build` creates `dist`, so the cache entry accepts a tree without `dist` (a clean checkout) and restores it on a hit. A `dist` that exists may be missing some of its entries, which the hit restores, but may not contain anything the build did not leave there: a stale chunk makes the entry miss, and the real build removes the chunk. A build on a tree that already has `dist` empties it and fills it again, so `dist` holds only what the build wrote, and that entry catches a stale file too.

## `vt run build`

cache miss, creates dist/

```
$ vite build
```

## `vt run build`

cache hit: dist/ is as the build left it

```
$ vite build ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt rm -rf dist`

a clean checkout has no dist/

```
```

## `vt run build`

cache hit: a directory the build created may be missing

```
$ vite build ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt stat-file dist/assets/main.js`

restored from the cache

```
dist/assets/main.js: exists
```

## `vtt rm dist/assets/main.js`

one chunk deleted from dist/

```
```

## `vt run build`

cache hit: a directory the build created may be missing some of its entries

```
$ vite build ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt stat-file dist/assets/main.js`

restored from the cache

```
dist/assets/main.js: exists
```

## `vtt write-file dist/assets/stale.js stale`

a chunk left by an older build

```
```

## `vt run build`

cache miss: dist/ is not as the build left it

```
$ vite build ○ cache miss: 'stale.js' added in 'dist/assets', executing
```

## `vtt stat-file dist/assets/stale.js`

the real build emptied dist/

```
dist/assets/stale.js: missing
```

## `vtt write-file dist/stale.html stale`

a page left by an older build

```
```

## `vt run build`

cache miss: dist/ held only what the build wrote, so it may not hold anything else

```
$ vite build ○ cache miss: 'stale.html' added in 'dist', executing
```

## `vtt stat-file dist/stale.html`

the real build emptied dist/ again

```
dist/stale.html: missing
```
