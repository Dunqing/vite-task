# read_write_task_is_cached_until_the_file_changes

A task that reads and writes the same file is cached. The file's content after the run is its fingerprint: a second run hits, because the file is as the task left it. Editing the file back to its original content is a miss, and the task rewrites it.

## `vt run task`

cache miss: replaces the first `i` with `!`

```
~/packages/rw-pkg$ vtt replace-file-content src/data.txt i !
```

## `vtt print-file src/data.txt`

the task's rewrite

```
or!ginal
```

## `vt run task`

cache hit: the file is as the task left it

```
~/packages/rw-pkg$ vtt replace-file-content src/data.txt i ! ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt print-file src/data.txt`

a hit does not rewrite it again

```
or!ginal
```

## `vtt write-file src/data.txt original`

edit the file back

```
```

## `vt run task`

cache miss: the file changed since the cached run

```
~/packages/rw-pkg$ vtt replace-file-content src/data.txt i ! ○ cache miss: 'packages/rw-pkg/src/data.txt' modified, executing
```

## `vtt print-file src/data.txt`

```
or!ginal
```
