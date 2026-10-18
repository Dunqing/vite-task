# node_compile_cache_does_not_poison_workspace

Runs a small Node script that turns on Node's compile cache. The cache should land in the OS temp directory (outside the workspace), so two `vt run --cache build` calls should be a miss then a hit. On Windows, if the spawned task env doesn't have `LOCALAPPDATA`, Node puts the cache inside the workspace instead, where the runner would track the cache files the script writes and reads; the harness forwards the env so the cache stays outside.

## `vt run --cache build`

first run: cache miss

```
$ node run.mjs
done
```

## `vt run --cache build`

second run: cache hit

```
$ node run.mjs ◉ cache hit, replaying
done

---
vt run: cache hit.
```
