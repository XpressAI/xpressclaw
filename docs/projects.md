# Projects and deletion

A Project is XpressClaw's collaboration and memory boundary. It owns its
Agents, Conversations, Tasks, Project memory, and the runs and schedules that
were started for that Project. Reusable workflow definitions and connectors
can be shared by several Projects and are not owned by one Project merely
because it used them.

## Permanently delete a Project

Open the Project, choose **Project settings → Delete project**, review the
current record counts, and type the Project name. The confirmation is
deliberately explicit because the operation cannot be undone.

Confirmed deletion:

- cancels active task attempts, queued work, Conversation turns, workflow
  runs, waits, and schedules;
- stops and removes XpressClaw runtime containers associated with the
  Project;
- removes every Project Agent from `xpressclaw.yaml` and from local
  collaboration-service access assignments;
- deletes the Project's Tasks, task messages and attachments, Conversations,
  messages, attachments and turns, Project memory, workflow runs, owned
  schedules, sync state, and other Project runtime records; and
- removes only the deleted Project's association with reusable workflows.
  Shared/global workflow definitions and connectors remain available.

XpressClaw never deletes a source repository or arbitrary host workspace as a
side effect of Project deletion. Workspace folders and repository files remain
on disk. Unmounted files that existed only inside a removed runtime container
are not recoverable, so copy anything important out of the container first.

Deletion is a recoverable two-phase operation. XpressClaw first marks the
Project as deleting and stops accepting new Tasks, messages, memory, workflow
work, schedules, or Agent assignments. It then cleans up processes,
containers, configuration, runtime files, and durable database rows. If
Docker/Podman is unavailable, the configuration file is not writable, or
another cleanup step fails, the Project remains visible with an interrupted
deletion warning. Correct the reported problem and confirm deletion again;
the retry resumes cleanup without creating duplicate work.

## API safety contract

An ordinary request remains non-destructive for populated Projects:

```http
DELETE /api/projects/{canonical-project-id}
```

It deletes only an empty Project and returns `409 Conflict` when owned work is
still present. API clients must send the exact acknowledgement to request a
cascade:

```http
DELETE /api/projects/{canonical-project-id}?cascade=confirmed
```

A successful deletion returns `204 No Content`. An unknown Project returns
`404 Not Found`, and an invalid acknowledgement returns `400 Bad Request`.
Cleanup failures return an actionable `500` or `503` response and leave the
durable deletion marker in place so the confirmed request can be retried
safely. Repeating a request after the Project was already removed returns the
accurate `404 Not Found` response and does not affect another Project.
