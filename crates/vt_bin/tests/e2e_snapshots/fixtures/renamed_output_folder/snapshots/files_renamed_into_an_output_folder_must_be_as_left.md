# files_renamed_into_an_output_folder_must_be_as_left

The build writes `dist.tmp/a.js` and renames `dist.tmp` to `dist`. The task created `dist`, but it wrote `a.js` under another name, so a hit can't rebuild it: the entry hits only while `dist/a.js` is as the build left it, instead of restoring an empty `dist`.

## `vt run build`

cache miss, renames dist.tmp/ to dist/

```
$ node build.mjs
```

## `vt run build`

cache hit: dist/ is as the build left it

```
$ node build.mjs ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt write-file dist/a.js stale`

a file left by an older build

```
```

## `vt run build`

cache miss: a hit can't rebuild dist/a.js, so it must be as the build left it

```
$ node build.mjs ○ cache miss: 'dist/a.js' modified, executing
```

## `vtt rm dist/a.js`

the file deleted

```
```

## `vt run build`

cache miss: a hit couldn't restore it

```
$ node build.mjs ○ cache miss: 'a.js' removed from 'dist', executing
```

## `vtt print-file dist/a.js`

rebuilt by the real build

```
built
```
