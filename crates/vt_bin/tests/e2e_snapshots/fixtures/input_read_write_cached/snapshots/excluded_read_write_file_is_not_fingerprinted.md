# excluded_read_write_file_is_not_fingerprinted

Excluding the file from `input` and `output` keeps it out of the fingerprint and the outputs: editing it between runs keeps the hit, and the hit leaves the edit alone.

## `vt run task-excluded`

```
~/packages/rw-pkg$ vtt replace-file-content src/data.txt i !
```

## `vtt write-file src/data.txt edited`

```
```

## `vt run task-excluded`

cache hit: the excluded file is not an input

```
~/packages/rw-pkg$ vtt replace-file-content src/data.txt i ! ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt print-file src/data.txt`

```
edited
```
