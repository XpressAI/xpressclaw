# Project storage and cleanup

Open a Project and choose **Clean up**. The inventory shows its registered Git
worktrees, their branches, measured size, and why each worktree is eligible or
kept. Worktrees created directly with Git are discovered too. **Select eligible
worktrees**, review the selection, then **Confirm cleanup**.

Cleanup removes selected directories and their generated caches. Git branches
and commit history remain. It rechecks the preview, Git status, repository
ownership, and runtime activity before deleting. Busy Agents defer cleanup;
running shell processes and helper-container mounts also protect worktrees.

The primary checkout, configured Agent workspaces, active repositories, locked
worktrees, and branches not merged into the local default-branch reference are
kept. Git references are inspected locally without fetching. Modified or
untracked files protect a worktree. Ignored personal files such as `.env` also
protect it; recognised generated directories such as `node_modules`,
`.svelte-kit`, and Cargo targets with `CACHEDIR.TAG` may be removed. Nested Git
repositories inside those caches are protected. Clean submodules can be removed
only when all their references are already reachable from remote references.

## Automatic retention

Automatic cleanup is off initially. Enable it in the same dialog and choose a
retention period of 1–365 days; the initial value is 14 days. XpressClaw checks
hourly and removes eligible worktrees beneath the repository's `.worktrees/`
directory. Other registered worktrees inside the approved workspace are offered
for manual cleanup.

The retention clock starts when an eligible worktree is first observed. A scan
that detects changes or activity resets that clock. The policy and observations
are stored in the local instance database and survive restarts; they are not
part of portable Project sync.

## Container storage

Project-owned recovery snapshots appear separately and require explicit
selection. They can contain old Agent filesystem state, so automatic worktree
retention does not delete them. Images referenced by running or stopped
containers, other images, Agent configuration, or extra tags are kept. Removal
uses exact owned image IDs without force or parent-image pruning.

The dialog also shows engine-wide image and volume usage. Shared image layers
are counted by the engine; per-snapshot reclaimable sizes use its unique-layer
information when available. Worktree size scans are bounded and show a lower
bound when incomplete. Volumes and images without Project ownership remain
outside Project cleanup.
