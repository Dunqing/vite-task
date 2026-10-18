# single_O_RDWR_open_is_cached

Opening a file with `O_RDWR` (e.g. `touch` keeping the file) reads and writes it in one call. The run is cached, and a second run hits.

## `vt run task`

```
~/packages/touch-pkg$ vtt touch-file src/data.txt
```

## `vt run task`

```
~/packages/touch-pkg$ vtt touch-file src/data.txt ◉ cache hit, replaying

---
vt run: cache hit.
```
