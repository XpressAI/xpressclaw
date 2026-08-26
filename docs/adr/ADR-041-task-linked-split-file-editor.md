# ADR-041: Task-Linked Split File Editor

## Status

Accepted

## Context

ADR-034 added an Agent Files view with a workspace tree, Monaco editor, Git
status, per-file diffs, and links from a Task's current workspace changes. A
changed-file link currently replaces the Task with the Agent Files view. A user
can manually split the Task first, but compact Task panes hide the Task details
sidebar and the Files view always reserves space for its tree.

That workflow makes it difficult to inspect or edit a changed file while
continuing to read and communicate with the Agent in the Task. The user must
manually assemble the layout and still loses useful Task context and editor
space.

## Decision

Changed-file links in a Task will open the selected workspace file in a new
right-side workspace pane when pane capacity and layout width allow it.

- The original Task remains open in its pane.
- The Task details sidebar remains visible in split layouts.
- The Agent Files view opens the selected file in Monaco with its workspace
  tree initially collapsed.
- The user can reopen the tree and browse other workspace files normally.
- Modified link activation, including Command/Ctrl, Shift, and Alt clicks,
  retains normal browser behavior.
- Split eligibility is based on measured workspace width rather than device or
  user-agent detection. Both the Task, including its details sidebar, and the
  file editor must retain usable widths.
- On mobile, constrained windows, or when the workspace pane limit has been
  reached, the link falls back to normal in-workspace navigation and does not
  create another pane.
- Direct Agent Files URLs continue to show the tree unless they explicitly
  request the collapsed state.

The implementation extends the existing workspace pane navigation and Agent
Files route state. It reuses ADR-034's workspace, Git, file, diff, revision,
and save APIs without changing their security or persistence boundaries.

## Architecture boundary

ADR-034 remains authoritative for workspace access, file editing, revision
conflicts, Git status, diffs, terminals, and security limits. This decision
only changes how the frontend composes the accepted Task, pane, and Files
surfaces.

The selected path and initial tree visibility are reconstructable from the
workspace tab route. No second editor, pane manager, backend endpoint, or
global editor state is introduced.

## Non-goals

- Adding or changing backend workspace APIs.
- Creating a separate editor or split-view system.
- Attributing workspace changes to a specific Task.
- Adding file creation, deletion, rename, or search.
- Redesigning the general workspace pane manager.
- Changing terminal behavior.
- Combining this work with ADR-039 discovery changes.

## Consequences

### Positive

- Users can inspect and edit a changed file without losing the Task transcript,
  composer, or details.
- Collapsing the tree gives Monaco more space while keeping workspace browsing
  one action away.
- The feature composes existing frontend architecture and keeps ADR-034's
  concurrency and security behavior intact.
- Normal links remain available as a graceful fallback and for modified-click
  browser workflows.

### Negative

- A Task pane with its details sidebar visible has less room for the transcript
  after splitting.
- Route state gains an explicit tree-visibility option.
- On mobile, constrained layouts, or at the pane limit, changed files replace
  the focused view instead of opening beside the Task.

## Acceptance criteria

1. Clicking a Task changed-file link opens the correct file in a right-side
   workspace pane when capacity allows.
2. The original Task, composer, and Task details sidebar remain visible.
3. Monaco opens with the workspace tree collapsed.
4. The user can reopen the tree and browse another file.
5. Code, diff-only deleted-file selection, saving, unsaved-change protection,
   and revision-conflict behavior remain unchanged.
6. Modified clicks retain normal browser link behavior.
7. Mobile, constrained-width, and pane-limit cases remain at one fewer pane,
   fall back to normal navigation, and do not overflow.
8. Direct and restored Agent Files routes reconstruct the selected path and
   requested tree state.

## Relationship to earlier decisions

This ADR builds on ADR-034 and does not modify or supersede its workspace,
editor, terminal, or security decisions. It supersedes only the implicit
frontend behavior in which Task changed-file links always replace the current
Task view.

## Visual evidence

- [Task and collapsed file editor](../assets/adr-041-task-file-split-collapsed.png)
- [File tree reopened](../assets/adr-041-task-file-split-tree-open.png)
- [Constrained-width navigation fallback](../assets/adr-041-constrained-navigation.png)
