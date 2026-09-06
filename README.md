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

This pulls a prebuilt binary from the GitHub Releases page for `darwin-arm64`,
`linux-x64`, and `linux-arm64` — no compile step. Intel macOS is not prebuilt;
use `cargo install --path .` to build from source. From v0.3.12 onward,
`cargo binstall jj-hooks` pulls from the standalone repository's Releases; older
pins resolve against the zireael monorepo and, once it goes private, fall back
to a source build.

### Via Homebrew tap

```bash
brew tap mattwilkinsonn/tap
brew install mattwilkinsonn/tap/jj-hooks
```

### Migrating from the zireael monorepo tap

The zireael monorepo tap hosts both `jj-hooks` and `jj-gt`, and Homebrew will
not untap a tap while any of its formulae are still installed. So uninstall
every tool you got from that tap before untapping, then install from the
consolidated tap:

```bash
brew uninstall jj-hooks jj-gt   # only the ones you actually installed
brew untap mattwilkinsonn/zireael
brew tap mattwilkinsonn/tap
brew install mattwilkinsonn/tap/jj-hooks mattwilkinsonn/tap/jj-gt
```

For a per-tool tap, replace `mattwilkinsonn/zireael` with that tap's name.

For `cargo binstall`, versions through 0.3.11 have `{repo}`-relative
download URLs and crates.io `repository` metadata pointing at the zireael
monorepo, which is going private — once it does, those old versions fall back to
a slow source build. v0.3.12 is the first release re-anchored at this standalone
repository.

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

## jjui integration

`jj-hp init` (and the standalone TOML below) installs two
[jjui](https://github.com/idursun/jjui) actions and keybindings so
`jj-hp push` is one keypress away from inside the TUI.

| Action | Default key | What it does |
| --- | --- | --- |
| `jj-hp-push-selected` | `x p` | Push the bookmark at the focused commit (`jj-hp push -r context.commit_id()`) |
| `jj-hp-push` | `x P` | Push every local bookmark across the repo (`jj-hp push --all`) |

Lowercase is the per-cursor case (just the focused bookmark);
uppercase is the "push everything" case. The 2026-05 swap moved
the keys to this layout and 2026-06 broadened `x P` to use
`--all` (jjui's cursor moves freely without `@`, so an
@-anchored "push everything" key never matched intent).
`jj-hp init` migrates older configs in place — user-customized
key sequences and lua bodies are left alone.

If you'd rather hand-edit `~/.config/jjui/config.toml` instead of
running `jj-hp init`, append:

```toml
[[actions]]
name = "jj-hp-push-selected"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push", "-r", context.commit_id())
  revisions.refresh()
"""

[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push", "--all")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push-selected"
seq = ["x", "p"]
scope = "revisions"
desc = "jj-hp push selected bookmark"

[[bindings]]
action = "jj-hp-push"
seq = ["x", "P"]
scope = "revisions"
desc = "jj-hp push every bookmark"
```

The `revisions.refresh()` after each `jj_async` repaints jjui's
revisions pane so the bookmark moves are visible immediately.

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
| `--stage <pre-commit\|pre-push>` | `pre-push` | Which hook stage to run |
| `--advance-bookmarks` | from config | Move local bookmarks to fixup commits on autofix |
| `--dry-run` | off | Forwarded to `jj git push` |
| anything after `--` | — | Forwarded verbatim to `jj git push` |

`run` flags:

| Flag | Default | Effect |
| ---- | ------- | ------ |
| `--stage <pre-commit\|pre-push>` | `pre-commit` | Which hook stage to run |
| `--all-files` | off | Run every hook against every tracked file, ignoring the revset's diff range. Maps to each runner's own all-files mode (see [Setup steps](#setup-steps)). |
| positional `REVSET` | `@` | Revset to check |

## Runner autodetection

`jj-hooks` probes the workspace root for these files, in order:

1. `hk.pkl` → `hk`
2. `lefthook.{yml,yaml,json,jsonc,toml}`, the dotted `.lefthook.*` forms, and the same names under `.config/` → `lefthook`
3. `.pre-commit-config.yaml` / `.pre-commit-config.yml` → `pre-commit`
4. `prek.toml` / `.prek.toml` → `prek` (prek's native TOML config)

If multiple match, `jj-hooks` errors out and asks for `--runner` — except for
the pre-commit / prek pair, which collapse to `prek` since `prek` reads both
formats. When only `.pre-commit-config.yaml` matches and `prek` is resolvable
(see [Runner binary resolution](#runner-binary-resolution) below), `jj-hooks`
silently uses `prek` (it's a faster drop-in). Override with
`--runner pre-commit` to force the slower path.

If no config matches, `jj-hp push` falls through to plain `jj git push` and
`jj-hp run` prints `no hook-runner config in target commit; skipping hooks`.

## Runner binary resolution

Once a runner is picked, `jj-hp` looks for the actual binary in the
following order. First hit wins; if everything misses, you get a
structured error naming the missing binary instead of a libc-level
`No such file or directory`.

1. **`jj-hooks.runner-bin.<runner>` config.** Explicit override. Set in
   your jj user, repo, or workspace config:

   ```toml
   [jj-hooks.runner-bin]
   prek = ".venv/bin/prek"                  # scalar: absolute or relative-to-workspace
   pre-commit = ["uv", "run", "--", "pre-commit"]   # array: wrapper + args
   ```

   Relative paths in the scalar form are resolved against the workspace
   root. The array form is taken verbatim and is the right shape for
   wrappers like `uv run`, `poetry run`, `nix shell -c`, etc.

2. **`.git/hooks/<stage>` shim path.** When you've run `prek install`
   or `pre-commit install`, the shim bakes the absolute path to the
   resolved binary into the hook file. `jj-hp` parses it out and uses
   the same path, so `jj-hp push` invokes the same runner that `git
   commit` / `git push` would. The two formats differ:
   - prek bakes `PREK="…/.venv/bin/prek"` and is invoked directly.
   - pre-commit bakes `INSTALL_PYTHON=…/.venv/bin/python` and is
     invoked as `python -mpre_commit` (i.e. the Python interpreter is
     the resolved binary; pre-commit is invoked as a module).
   Both shapes are handled.

3. **`uv run` wrapping.** When `workspace_root/uv.lock` exists *and*
   `uv` is on `$PATH`, prek / pre-commit invocations are prefixed with
   `uv run --project <workspace_root> --`. uv resolves the project's
   venv automatically; you don't have to activate anything. The
   `--project` flag points uv at your actual workspace rather than
   the ephemeral worktree jj-hp runs the hook in (since `.venv` is
   typically gitignored and wouldn't exist in the worktree). Only
   applies to prek / pre-commit (lefthook / hk aren't Python tools).

4. **`$PATH`.** The classic fallback — bare program name found via the
   shell's PATH walk.

If you see `hook runner ‹bin› is not on $PATH`, none of the four layers
matched. Either install the runner globally (`brew install prek`,
`pipx install prek`, `uv tool install prek`), activate your venv
before invoking `jj-hp`, or set `jj-hooks.runner-bin.<runner>`.

## Repo environment (direnv / devenv)

Hooks run in an ephemeral `/tmp` worktree, so by default the hook subprocess
inherits only `jj-hp`'s own process environment — **not** the repo's
direnv/devenv environment. That means tools your hooks shell out to (moon,
biome, proto shims, …) resolve against the *system* `$PATH` instead of the
versions your project pins, producing false-red gates (e.g. system biome
2.4.16 grading files your CI checks with devenv's 2.5.4) or hard failures.

`jj-hp` closes this gap: if the workspace has a `.envrc` and `direnv` is on
`$PATH`, it runs `direnv export json` **once** (against the workspace root,
where the `.envrc` is allowed — never the temp worktree), caches the result
for the whole invocation, and merges that environment into every hook and
setup-step subprocess before spawning. The local gate then runs in the same
environment CI does (`devenv shell -- devenv tasks run ci`). This works for any `.envrc`
— plain `direnv`, `use devenv`, and `source_env_if_exists .envrc.local`
overrides alike — because it goes through direnv itself.

The merge is a **diff**, not a replacement (direnv's export is a diff against
the invoking environment), so all three launch states behave correctly:

- Launched from a shell that already has this repo's env loaded → the diff is
  empty, behavior is unchanged.
- Launched from a bare environment (e.g. an automation harness) → the diff
  prepends the devenv `PATH` and sets the devenv variables.
- Launched from a shell with a *different* repo's env loaded → direnv reverses
  that repo's env before applying this one.

Git repo-location variables (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, and
the rest of the family git reports via `git rev-parse --local-env-vars`) are
stripped from the hook child unconditionally — whether they arrive via the
patch or are inherited from an already-loaded shell (a secondary workspace's
`.envrc.local`): the child must run git against the temp worktree it is checked
out in, not against whatever a `.envrc.local` might point `GIT_DIR` at.

When the workspace has a `.envrc` that is present but not yet `direnv allow`ed
(the usual state of a fresh clone), `direnv export` reports the environment as
*blocked* and the gate would otherwise run env-blind against the system
`$PATH`. `jj-hp` closes this too: it runs `direnv allow` on the workspace root
and re-exports, so the gate gets the repo's pinned toolchain on the very first
push instead of after a manual bootstrap step. This mutates direnv's global
trust database, so — as with any `direnv allow` — direnv will thereafter
auto-load that `.envrc` in your interactive shells in that directory too. The
auto-allow is never fatal: any failure warns once and falls back to the prior
blocked behavior (hooks run without the repo env). Turn it off with
`JJ_HOOKS_NO_DIRENV_ALLOW` / `jj-hooks.repo-env-autoallow = false` (below).

### Fallback and failure semantics

This is a strict superset of the previous behavior — it never introduces a new
failure mode. When there is **no `.envrc`**, **no `direnv` on `$PATH`**, or the
export fails for any other reason, the hook subprocess environment is exactly
what it was before (the parent env plus `JJ_HOOKS_WORKSPACE`). Env-load
failures are never fatal:

- A **blocked `.envrc`** (a fresh clone where you haven't run `direnv allow`
  yet) is auto-`direnv allow`ed by jj-hp so the first push already runs with the
  repo env; see above for the trust-DB side effect and the off-switch. Should
  the auto-allow itself fail, jj-hp prints a one-line notice and runs hooks
  without the repo env.
- A **stale/corrupt inherited `DIRENV_DIFF`** is retried once with the
  `DIRENV_*` state cleared before falling back.
- Any other export failure logs a `tracing` warning and falls back.

### Opting out

The mechanism is automatic, with two off-switches:

- **`JJ_HOOKS_NO_REPO_ENV=1`** environment variable (any non-empty value).
- **`jj-hooks.repo-env = "off"`** in jj config (`"auto"` or unset =
  automatic). Set in your user, repo, or workspace config, e.g.
  `jj config set --repo jj-hooks.repo-env off`.
- **`JJ_HOOKS_NO_DIRENV_ALLOW=1`** environment variable (any non-empty value)
  disables just the auto-`direnv allow` of a blocked `.envrc` — an already
  allowed `.envrc` is still exported and merged as usual.
- **`jj-hooks.repo-env-autoallow = false`** in jj config (`true` or unset =
  automatic) is the config equivalent, e.g.
  `jj config set --repo jj-hooks.repo-env-autoallow false`.

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

## Setup steps

`git worktree add --detach` checks out the tracked tree only — gitignored
content like `node_modules/`, `.venv/`, `target/` is absent. Hooks that
depend on those resources (e.g. `tsc`, `pytest`, `cargo nextest`) fail
inside the ephemeral worktree with `command not found` or `module not found`.

Configure `jj-hooks.setup` to declare commands jj-hp runs inside the
worktree *before* the hook runner fires.

### Rust build cache

The absent `target/` is a special case: rather than rebuild the whole crate
cold in every ephemeral worktree (~35s even with sccache), `jj-hp` points the
gate's `CARGO_TARGET_DIR` at the **primary** repo's `target/` so cargo reuses
your own warm dev builds. The gate then finishes near-instantly instead of
paying a cold build, on the first gated push as well as later ones. This is set
unconditionally on every hook and setup-step subprocess (after the repo env, so
it always wins) — harmless for non-cargo repos, where nothing reads
`CARGO_TARGET_DIR`. `hk validate` is not affected. Turn it off with
`JJ_HOOKS_NO_GATE_CACHE` / `jj-hooks.gate-cache = "off"`:

- **`JJ_HOOKS_NO_GATE_CACHE=1`** environment variable (any non-empty value).
- **`jj-hooks.gate-cache = "off"`** in jj config (`"auto"` or unset = automatic),
  e.g. `jj config set --repo jj-hooks.gate-cache off`.

### Quick start

The fastest way to add a setup step is `jj config set --repo`, which writes
the value into the repo's config without you having to find or open the
file:

```bash
# Single step: `bun install` before every hook run.
jj config set --repo 'jj-hooks.setup' \
  '[{ name = "install deps", run = ["bun", "install", "--frozen-lockfile"] }]'

# Verify what landed:
jj config get jj-hooks.setup

# Remove it later:
jj config unset --repo jj-hooks.setup
```

Multi-step setup is the same call — `jj config set` takes the whole value
as one TOML expression. Wrap multiple inline tables in `[ … ]`:

```bash
jj config set --repo 'jj-hooks.setup' \
  '[
    { name = "install deps", run = ["bun", "install", "--frozen-lockfile"] },
    { name = "codegen", run = ["bun", "run", "prepare"] },
  ]'
```

For long / multi-step configs the file form is easier to read. `jj config
path --repo` prints the repo config's path (creating it if missing); edit
that file directly:

```bash
$EDITOR "$(jj config path --repo)"
```

```toml
# .jj/repo/config.toml
[[jj-hooks.setup]]
name = "install deps"
run = ["bun", "install", "--frozen-lockfile"]

[[jj-hooks.setup]]
name = "codegen"
run = ["bun", "run", "prepare"]
```

User-level (apply to every repo): swap `--repo` for `--user` on every
command, or edit `~/.config/jj/config.toml`. Repo-level overrides user-level
when both define the same key.

### Step shape

Each entry:

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `name` | string | no | Label used in failure messages. Falls back to `run[0]`. |
| `run` | array of strings | yes | argv list — exec'd directly, no shell. |

`run` is an argv list (not a shell string) so quoting rules can't bite. For
chained commands write `["bash", "-c", "foo && bar"]` explicitly.

Steps run in declared order. A non-zero exit aborts the pipeline before the
hook runner is invoked — there's no point grading a broken worktree.

### `JJ_HOOKS_WORKSPACE`

Both setup steps and hook subprocesses see `JJ_HOOKS_WORKSPACE` in their
environment, pointing at the workspace `jj-hp` was invoked from (primary or
secondary). Use it to reach back into the invocation workspace's resources:

```toml
# Hardlink-copy node_modules from the invocation workspace instead of
# running a full install. Cheap on Linux (hardlinks are O(file count) metadata
# ops); falls through to `cp -a` on macOS where -al isn't supported by default.
[[jj-hooks.setup]]
name = "share node_modules"
run = ["bash", "-c", "cp -al \"$JJ_HOOKS_WORKSPACE/node_modules\" . 2>/dev/null || cp -a \"$JJ_HOOKS_WORKSPACE/node_modules\" ."]
```

The retry-after-fixup pass (issue jj-hooks#11) re-creates the worktree, so
setup steps run again on the retry — important when the hook's first run
mutates state that the setup needs to restore.

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
| `jj-hooks.setup` | array of tables | empty | Pre-hook setup steps; see [Setup steps](#setup-steps) |

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
direnv allow              # one-time: devenv shell provides the hook-runner stack (pre-commit, prek, lefthook, hk, pkl)
devenv tasks run ci       # fmt + clippy + nextest
```

The test suite includes integration tests that build real jj+git repos in
tempdirs, install local pre-commit hooks, and run the full push pipeline —
including the secondary-workspace path. Every supported runner (pre-commit,
prek, lefthook, hk) has dedicated integration tests for pass/fail/autofix.

## License

This project is dual-licensed under your choice of MIT
([`LICENSE-MIT`](./LICENSE-MIT)) or Apache-2.0
([`LICENSE-APACHE`](./LICENSE-APACHE)).
