# created_dist_may_be_missing_or_incomplete

`vite build` creates `dist`, so the cache entry accepts a tree without `dist` (a clean checkout) and restores it on a hit, and a `dist` missing some of its entries, which the hit restores. A hit doesn't delete: a chunk an older build left in `dist` stays after a hit, until `dist` is deleted.

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

cache hit: a hit doesn't delete files

```
$ vite build ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt stat-file dist/assets/stale.js`

still there

```
dist/assets/stale.js: exists
```

## `vtt rm -rf dist`

delete dist/ to get a clean one

```
```

## `vt run build`

cache hit: restores dist/

```
$ vite build ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt stat-file dist/assets/stale.js`

gone: the hit restored only what the build wrote

```
dist/assets/stale.js: missing
```
