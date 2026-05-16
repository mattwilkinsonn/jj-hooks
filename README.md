# jj-hooks

Run [pre-commit](https://pre-commit.com/), [prek](https://github.com/j178/prek),
[lefthook](https://github.com/evilmartians/lefthook), or
[hk](https://hk.jdx.dev) hooks against [jj](https://jj-vcs.github.io) bookmark
pushes — with full support for secondary jj workspaces.

Ships as two binaries: `jj-hooks` (canonical name) and `jj-hp` (shorter alias
that's easier to type and works with shell completion). Pick the one you like;
they're identical.

## What it does

`jj-hp push` is a drop-in replacement for `jj git push`:

1. Asks jj which bookmarks the push would update on the remote.
2. For each bookmark being added or moved, creates an ephemeral detached git
   worktree at the target commit and runs the configured hook backend there.
3. If hooks fail or modify files, the push is aborted. Modifications get
   committed as a fixup commit whose hash is printed so you can `jj squash`
   the fixes into your target or inspect them with `jj show`.
4. If everything passes cleanly, executes the real `jj git push`.

`jj-hp run [REVSET]` runs hooks against a revset without pushing — useful for
"lint this change before I move on" workflows.

## Why a worktree?

Earlier jj + pre-commit integrations ran hooks in the user's working copy,
which doesn't work from a secondary workspace: the worktree is the secondary's
files but the git index lives in the primary's `.git`, so pre-commit's
"`.pre-commit-config.yaml` is unstaged" check fires every time.

`jj-hooks` sidesteps this entirely by running every hook in a fresh
`git worktree add --detach` checkout of the target commit. The user's
working copy is never touched, and the same code path works in both
primary and secondary workspaces.

## Prior art

- [jj-pre-push](https://github.com/acarapetis/jj-pre-push) — the Python tool
  that originally inspired this. `jj-hooks` adopts its bookmark-update parsing
  strategy and broadens the runner support.
- <https://www.aazuspan.dev/blog/automating-pre-push-checks-with-jujutsu/>
- Discussion on <https://github.com/jj-vcs/jj/issues/405>

## Installation

### Via cargo binstall (recommended)

```bash
cargo binstall jj-hooks
```

This pulls a prebuilt binary from the GitHub Releases page — no compile step.

### Via Homebrew tap

```bash
brew install mattwilkinsonn/tap/jj-hooks
```

### From source

```bash
jj git clone https://github.com/mattwilkinsonn/jj-hooks
cargo install --path .
```

After install run the interactive setup (optional):

```bash
jj-hp init
```

This prompts to:

- Install a user-level `jj push` alias that delegates to `jj-hp push`.
- Enable `jj-hooks.advance-bookmarks` so the local bookmark automatically moves
  to the fixup commit when hooks autofix something.
- Install jjui actions/bindings so `jj-hp push` is reachable from inside jjui.

All three can be reconfigured by running `jj-hp init` again.

## Shell completion

`jj-hp completions <shell>` emits a clap-generated completion script. The
script wires dynamic completers for `--bookmark` and `--remote` that shell
out to `jj` to enumerate live values.

```bash
# zsh: add to ~/.zshrc
eval "$(jj-hp completions zsh)"

# bash: add to ~/.bashrc
eval "$(jj-hp completions bash)"

# fish: write to ~/.config/fish/completions/jj-hp.fish
jj-hp completions fish > ~/.config/fish/completions/jj-hp.fish
```

After that, `jj-hp push -b <TAB>` will complete bookmark names from your repo
and `jj-hp push --remote <TAB>` will complete remote names.

> **Note**: completion only works for `jj-hp` directly. The `jj push` alias
> (installed by `jj-hp init`) runs through jj's own completion script, which
> doesn't expand user aliases — so `jj push -b <TAB>` won't complete bookmark
> names. Use `jj-hp push -b <TAB>` instead. This is a limitation of jj's
> completion script, not jj-hooks.

## Usage

```text
jj-hp push [-b BOOKMARK]... [--remote REMOTE] [other flags] [-- JJ_GIT_PUSH_ARGS...]
jj-hp run  [--stage pre-commit|pre-push] [REVSET]
jj-hp init
jj-hp completions <bash|zsh|fish|powershell>
```

Global flags:

| Flag | Env | Default | Effect |
| ---- | --- | ------- | ------ |
| `--runner <pre-commit\|prek\|lefthook\|hk>` | `JJ_HOOKS_RUNNER` | autodetect | Override runner selection |
| `--log-level <level>` | `JJ_HOOKS_LOG` | `warn` | tracing-subscriber filter |

`push` flags (mirrors `jj git push`):

| Flag | Default | Effect |
| ---- | ------- | ------ |
| `-b/--bookmark NAME` | — | Push only this bookmark; repeatable |
| `-r/--revision REVSET` | — | Push bookmarks pointing at these commits; repeatable |
| `-c/--change REVSET` | — | Push these commits by creating a bookmark; repeatable |
| `--remote NAME` | — | The remote to push to |
| `--all` | off | Push all bookmarks (including new ones) |
| `--tracked` | off | Push all tracked bookmarks |
| `--deleted` | off | Push all deleted bookmarks |
| `--allow-new` | off | Allow pushing new (untracked) bookmarks |
| `--stage <pre-commit\|pre-push>` | `pre-push` | Which hook stage to run |
| `--advance-bookmarks` | from config | Move local bookmarks to fixup commits on autofix |
| `--dry-run` | off | Forwarded to `jj git push` |
| anything after `--` | — | Forwarded verbatim to `jj git push` |

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
never autodetected by file — it shares pre-commit's config file. Instead, if
the autodetected runner is `pre-commit` and `prek` is on `$PATH`, `jj-hooks`
silently uses `prek` (it's a faster drop-in). Override with `--runner pre-commit`
to force the slower path.

If no config matches, `jj-hp push` falls through to plain `jj git push`.

## Fixup commits

When hooks modify files in the ephemeral worktree, `jj-hooks` stages them,
writes a tree, builds a commit with the bookmark's current target as parent,
and anchors that commit under `refs/heads/jj-hooks-fixup/<bookmark>` just
long enough for `jj git import` to pick it up. Then it deletes both the
temp jj bookmark and the underlying git ref — the commit itself stays
fully addressable by hash in jj's commit graph.

The output of a push that produced a fixup looks like this:

```text
jj-hooks: Move forward main from abc12345 to def67890: hooks modified files (fixup commit 0123abcd...)
jj-hooks: aborting push
```

Copy the `0123abcd...` and decide what to do with it:

```bash
jj log -r 0123abcd      # inspect the fixup
jj squash --from 0123abcd --into main      # fold the fixes into main
```

With `--advance-bookmarks` (or `jj-hooks.advance-bookmarks = true` in config),
`jj-hooks` advances the local bookmark to the fixup commit automatically —
re-run `jj-hp push` to actually push the fixed version.

The push is always aborted when a fixup commit is created. Run `jj-hp push`
again after squashing/advancing.

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

## Using the `jj push` alias (optional)

If you came from
[jj-pre-push](https://github.com/acarapetis/jj-pre-push) or just prefer typing
`jj push`, `jj-hp init` can wire up an alias for you:

```toml
# Added to ~/.config/jj/config.toml by `jj-hp init`
[aliases]
push = ["util", "exec", "--", "jj-hp", "push"]
```

After that, `jj push` works exactly like `jj-hp push`. The catch is that
shell completion only sees `jj`'s own completion table, which doesn't expand
user-defined aliases — so `jj push -b <TAB>` won't complete bookmark names.
For that, fall back to `jj-hp push -b <TAB>`.

The recommended workflow is to use `jj-hp` directly. The alias exists for
muscle memory.

## Development

```bash
just install-deps   # install pre-commit, prek, lefthook, hk, markdownlint-cli2, actionlint
just test           # check-deps + cargo nextest
just ci             # fmt-check + clippy + test
```

The test suite includes integration tests that build real jj+git repos in
tempdirs, install local pre-commit hooks, and run the full push pipeline —
including the secondary-workspace path. Every supported runner (pre-commit,
prek, lefthook, hk) has dedicated integration tests for pass/fail/autofix.

## License

Apache-2.0.
