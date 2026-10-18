# multi_task_with_read_write_hits_on_rerun

In a multi-task (`-r`) run, tasks that read and write a file are cached like any other, so a second run is all hits.

## `vt run -r task`

```
~/packages/normal-pkg$ vtt print hello
hello

~/packages/touch-pkg$ vtt touch-file src/data.txt

~/packages/rw-pkg$ vtt replace-file-content src/data.txt i !

---
vt run: 0/3 cache hit (0%), 3/3 successful. (Run `vt run --last-details` for full details)
```

## `vt run -r task`

```
~/packages/normal-pkg$ vtt print hello ◉ cache hit, replaying
hello

~/packages/touch-pkg$ vtt touch-file src/data.txt ◉ cache hit, replaying

~/packages/rw-pkg$ vtt replace-file-content src/data.txt i ! ◉ cache hit, replaying

---
vt run: 3/3 cache hit (100%), 3/3 successful. (Run `vt run --last-details` for full details)
```
