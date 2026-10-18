# outputs_with_the_checkout_path_are_not_shared_across_paths

`prettier --check --cache .` writes `node_modules/.cache/prettier/.prettier-cache`, keyed by absolute file paths. An entry whose outputs contain the checkout path is reused only at that path: copying `a`'s task cache into the identical workspace `b` (as a remote cache would share it) misses there, and `b` records its own entry.

## `cd a && vt run format`

cache miss, writes the prettier cache

```
$ prettier --check --cache .
Checking formatting...
All matched files use Prettier code style!
```

## `cd a && vtt grep-file node_modules/.cache/prettier/.prettier-cache a.js`

the cache names the checked files by absolute path

```
node_modules/.cache/prettier/.prettier-cache: found "a.js"
```

## `cd a && vt run format`

cache hit at the same path

```
$ prettier --check --cache . ◉ cache hit, replaying
Checking formatting...
All matched files use Prettier code style!

---
vt run: cache hit.
```

## `vtt cp -r a/node_modules/.vite/task-cache b/node_modules/.vite/task-cache`

share the entry with another checkout

```
```

## `cd b && vt run format`

cache miss: the outputs embed a's path

```
$ prettier --check --cache . ○ cache miss: outputs embed the checkout path '<workspace>/a', executing
Checking formatting...
All matched files use Prettier code style!
```

## `cd b && vt run format`

cache hit: b's own entry

```
$ prettier --check --cache . ◉ cache hit, replaying
Checking formatting...
All matched files use Prettier code style!

---
vt run: cache hit.
```
