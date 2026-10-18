# read_write_outside_package_is_cached

The read and written file may be outside the task's package.

## `vt run task-outside`

```
~/packages/rw-pkg$ vtt replace-file-content ../../shared.txt i !
```

## `vt run task-outside`

```
~/packages/rw-pkg$ vtt replace-file-content ../../shared.txt i ! ◉ cache hit, replaying

---
vt run: cache hit.
```
