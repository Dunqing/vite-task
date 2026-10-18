# listed_folder_may_lack_entries_the_task_created

`eslint --cache .` lists the project directory to find files and creates `.eslintcache` in it. A listing must be as the task left it, except that an entry the task created may be missing, since a hit restores it: the second run hits, a clean checkout without `.eslintcache` hits too (and gets the cache file restored), and a new source file is a miss.

## `vt run lint`

cache miss, writes .eslintcache

```
$ eslint --cache .
```

## `vtt stat-file .eslintcache`

```
.eslintcache: exists
```

## `vt run lint`

cache hit: the listing is as the task left it

```
$ eslint --cache . ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt rm .eslintcache`

```
```

## `vt run lint`

cache hit on a clean checkout

```
$ eslint --cache . ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt stat-file .eslintcache`

restored from the cache

```
.eslintcache: exists
```

## `vtt write-file src/b.js 'export const b = 2;'`

a new source file

```
```

## `vt run lint`

cache miss: src/ has a new entry

```
$ eslint --cache . ○ cache miss: 'b.js' added in 'src', executing
```
