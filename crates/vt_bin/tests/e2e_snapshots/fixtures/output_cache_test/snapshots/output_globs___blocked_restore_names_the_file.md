# output_globs___blocked_restore_names_the_file

When a directory stands where an archived output file goes, restoring fails and names the file. It doesn't suggest clearing the cache, which wouldn't help.

## `vt run build`

first run — cache miss, writes dist/output.txt

```
$ vtt write-file dist/output.txt built
```

## `vtt rm -rf dist`

```
```

## `vtt mkdir -p dist/output.txt/stale`

a directory where the output file goes

```
```

## `vt run build`

cache hit — restoring fails and names the file

**Exit code:** 1

```
$ vtt write-file dist/output.txt built ◉ cache hit, replaying
✗ Cache restore failed: couldn't write `dist/output.txt`: <os error>
```
