# jj-hooks

Run [pre-commit](https://pre-commit.com/), [prek](https://github.com/j178/prek),
[lefthook](https://github.com/evilmartians/lefthook), or
[hk](https://hk.jdx.dev) hooks against [jj](https://jj-vcs.github.io) bookmark
pushes — with full support for secondary jj workspaces.

## What it does

`jj-hooks push` is a drop-in replacement for `jj git push`:

1. Asks jj which bookmarks the push would update on the remote.
2. For each bookmark being added or moved, creates an ephemeral detached git
   worktree at the target commit and runs the configured hook backend there.
3. If hooks fail or modify files, the push is aborted. Modifications get
   committed as a fixup commit anchored at
   `refs/heads/jj-hooks-fixup/<bookmark>` so the autofixes aren't lost.
4. If everything passes cleanly, executes the real `jj git push`.

`jj-hooks run [REVSET]` runs hooks against a revset without pushing — useful
for "lint this change before I move on" workflows.

## Why a worktree?

Earlier jj + pre-commit integrations ran hooks in the user's working copy,
which doesn't work from a secondary workspace: the worktree is the secondary's
files but the git index lives in the primary's `.git`, so pre-commit's
"`.pre-commit-config.yaml` is unstaged" check fires every time.

`jj-hooks` sidesteps this entirely by running every hook in a fresh
`git worktree add --detach` checkout of the target commit. The user's
working copy is never touched, and the same code path works in both
primary and secondary workspaces.

## Installation

```bash
cargo install --path .
```

Then run the interactive setup (optional but recommended):

```bash
jj-hooks init
```

This prompts to:

- Install a user-level `jj push` alias that delegates to `jj-hooks push`.
- Enable `jj-hooks.advance-bookmarks` so the local bookmark automatically moves
  to the fixup commit when hooks autofix something.

Both can be reconfigured by running `jj-hooks init` again.

## Usage

```text
jj-hooks push [-- JJ_GIT_PUSH_ARGS...]
jj-hooks run  [--stage pre-commit|pre-push] [REVSET]
jj-hooks init
```

Global flags:

| Flag | Env | Default | Effect |
| ---- | --- | ------- | ------ |
| `--runner <pre-commit\|prek\|lefthook\|hk>` | `JJ_HOOKS_RUNNER` | autodetect | Override runner selection |
| `--log-level <level>` | `JJ_HOOKS_LOG` | `warn` | tracing-subscriber filter |

`push` flags:

| Flag | Default | Effect |
| ---- | ------- | ------ |
| `--stage <pre-commit\|pre-push>` | `pre-push` | Which hook stage to run |
| `--advance-bookmarks` | from config | Move local bookmarks to fixup commits on autofix |
| `--dry-run` | off | Forwarded to `jj git push` |
| trailing args | — | Everything after recognized flags is forwarded to `jj git push` |

`run` flags:

| Flag | Default | Effect |
| ---- | ------- | ------ |
| `--stage <pre-commit\|pre-push>` | `pre-commit` | Which hook stage to run |
| positional `REVSET` | `@` | Revset to check |

## Runner autodetection

`jj-hooks` probes the workspace root for these files, in order:

1. `hk.pkl` → `hk`
2. `lefthook.yml` / `lefthook.yaml` / `.lefthook.yml` / `.lefthook.yaml` → `lefthook`
3. `.pre-commit-config.yaml` / `.pre-commit-config.yml` → `pre-commit`

If multiple match, `jj-hooks` errors out and asks for `--runner`. `prek` is
never autodetected (it shares pre-commit's config file); use `--runner prek`
or `JJ_HOOKS_RUNNER=prek` to opt in.

If no config matches, `jj-hooks push` falls through to plain `jj git push`.

## Fixup commits

When hooks modify files in the ephemeral worktree, `jj-hooks` stages them,
writes a tree, builds a commit with the bookmark's current target as parent,
and anchors that commit under `refs/heads/jj-hooks-fixup/<bookmark>`. Then it
runs `jj git import` so jj sees the new commit as a `jj-hooks-fixup/<name>`
bookmark.

By default the user's real bookmark stays put — you decide whether to squash
the fixup into the target or move the bookmark yourself:

```bash
jj log -r 'jj-hooks-fixup/main | main'   # inspect
jj squash --from jj-hooks-fixup/main --into main
# or
jj bookmark set main -r jj-hooks-fixup/main --allow-backwards
jj bookmark forget jj-hooks-fixup/main
```

With `--advance-bookmarks` (or `jj-hooks.advance-bookmarks = true` in config),
`jj-hooks` does the second sequence automatically: bookmark moves, temp
bookmark and ref are removed.

The push is still aborted whenever a fixup commit is created. Run `jj-hooks
push` again after squashing/advancing.

## Workspaces

`jj-hooks` resolves the primary git directory via
`.jj/repo/store/git_target`, following the `.jj/repo` pointer file in
secondary workspaces. All git plumbing (worktree creation, `commit-tree`,
`update-ref`) targets the primary `.git`, so commits and refs land in the
shared object database regardless of which workspace you ran from.

## Configuration

All config keys live under `jj-hooks.*` in jj's user/repo config:

| Key | Type | Default | Notes |
| --- | ---- | ------- | ----- |
| `jj-hooks.advance-bookmarks` | bool | false | Default for `--advance-bookmarks` |

`--runner` and `--stage` are command-line / env only — they belong with the
invocation, not the config.

## Development

```bash
just install-deps   # install pre-commit, prek, lefthook, hk (macOS via brew, Linux via uv/cargo)
just test           # check-deps + cargo nextest
just ci             # fmt-check + clippy + test
```

The test suite includes integration tests that build real jj+git repos in
tempdirs, install local pre-commit hooks, and run the full push pipeline —
including the secondary-workspace path.

## Prior art

- [jj-pre-push](https://github.com/acarapetis/jj-pre-push) — the Python tool
  that originally inspired this. `jj-hooks` adopts its bookmark-update
  parsing strategy and broadens the runner support.
- <https://www.aazuspan.dev/blog/automating-pre-push-checks-with-jujutsu/>
- Discussion on <https://github.com/jj-vcs/jj/issues/405>

## License

Apache-2.0.
