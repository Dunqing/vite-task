# git_state_as_an_explicit_input

`.git` is never tracked automatically, but typedoc puts the commit hash into its source links, so the task lists `.git/HEAD` and `.git/refs/**` as explicit inputs. A run without changes hits; a new commit changes `.git/refs/heads/main` and misses.

## `GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=.gitconfig-none git init -q -b main`

```
```

## `GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=.gitconfig-none git add -A`

```
```

## `GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=.gitconfig-none git -c user.name=vp -c user.email=vp@example.com commit -q -m initial`

```
```

## `vt run docs`

cache miss

```
$ typedoc
[info] html generated at ./docs
```

## `vtt grep-file docs/functions/add.html blob/`

the source link carries the commit

```
docs/functions/add.html: found "blob/"
```

## `vt run docs`

cache hit

```
$ typedoc ◉ cache hit, replaying
[info] html generated at ./docs

---
vt run: cache hit.
```

## `GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=.gitconfig-none git -c user.name=vp -c user.email=vp@example.com commit -q --allow-empty -m empty`

a new commit, no file changes

```
```

## `vt run docs`

cache miss: the branch's ref changed

```
$ typedoc ○ cache miss: '.git/refs/heads/main' modified, executing
[info] html generated at ./docs
```
