# Ports, container files, and shared terminals

These features belong to the Agent environment and work across the native
harnesses, including pi. Install the updated XpressClaw server and rebuild or
pull the updated runner image to get the new MCP tools and tmux. Custom images
need Node.js and tar for file/port access and tmux for shared terminal sessions.
Older images without tmux retain an ordinary interactive shell.

## Connect a local LLM

In the Agent's **Environment** tab, add a **Host → container** forward with
host port `8080` and container port `8080`. The harness can then use
`http://127.0.0.1:8080`, even when the LLM listens only on the host loopback
interface. Ports may differ if one side already uses that number.

“Host” means the machine running the XpressClaw server. With a remote Desktop
profile, this is the remote server, not the Desktop client's machine.

Mappings are saved in the local instance database. They start before the next
Agent prompt and when the environment is opened through Files or Terminal.
Adding a mapping to a running container applies it immediately. Removing it
stops the bridge and forgets the mapping. A stopped or recreated container
gets its mappings back when started through XpressClaw. An occupied listening
port is reported as an error when adding a mapping. On restoration, a failed
mapping stays inactive and its error is logged; other mappings and Agent turns
continue. Services can remain bound to `127.0.0.1`.

## Share a container server

Add **Container → host** in Environment, or let the Agent call:

```json
expose_port({"container_port": 3000, "host_port": 3000})
```

The user can access the server at `http://127.0.0.1:3000` on the XpressClaw
server machine. TCP is forwarded without interpreting HTTP, so WebSockets and
streamed LLM responses use the same connection. List and remove mappings with
`list_port_forwards` and `remove_port_forward`.

An exposed host port is reserved across Agents. Another Agent cannot reuse it
for a listener or import it as a host service; adding such a mapping returns a
conflict with the owning Agent and port. Multiple Agents may import the same
host LLM port when it is not an Agent's exposed listener. Listeners bind only
to loopback; remote clients need their usual access tunnel to the server machine.

The bridge uses an owned Docker/Podman exec stream instead of container IPs,
host networking, or container creation-time port bindings. It supports TCP,
up to 16 mappings per Agent and 64 concurrent connections per mapping. Connections
to either loopback target time out after 10 seconds if they cannot connect.
Startup and shutdown wait independently for each Agent. UDP is
not forwarded. Configuration is installation-local and excluded from Project
sync; deleting the Agent or Project removes its saved mappings.

## Browse and publish files

In **Files**, choose **Workspace** for the existing repository tree and Git
diffs, or **Container** to browse absolute directories such as `/tmp` and
`/home/node`. Text files can be edited with Monaco. Revision checks reject a
save if another process changed the file after it was opened.

Each file and directory has a download action. Folder downloads are `.tar.gz`
archives; the selected folder is the top-level archive entry. Symbolic links
inside an archive are preserved, not followed. The browser excludes kernel
and device paths (`/proc`, `/sys`, `/dev`). Text editing is limited to 4 MiB;
file downloads and compressed archives are limited to 100 MiB. Stopped retained
environments restart for access; an environment must have been initialized
by running a task at least once.

Agents should call `publish_task_files` with file or folder paths to copy
results into durable task attachments. Absolute container paths, including
`/tmp`, are supported. Up to eight items and 20 MiB total may be published per
message. File and folder sizes are checked before downloading; publication
limits both the uncompressed inputs and resulting attachments to 20 MiB.
Attachments remain downloadable after the container is removed.
`send_conversation_message` also accepts files and folder paths outside the
workspace. Larger output directories can be downloaded from Files.

Task chat accepts up to five uploaded files with a 20 MiB aggregate limit.
Supported raster images keep their 5 MiB per-image limit and are sent as ACP
images. Other files are staged in private temporary directories inside the
retained container, outside its workspace, so Git status and `git add -A` cannot
include them. The Agent receives the container path and original filename in
the prompt. Staged copies remain until the container is removed; the original
task attachments remain downloadable from the task.

Project conversation turns use the same staging path for files attached to the
triggering message. Supported images become ACP image blocks; PDFs and other
files are staged with their container paths in the prompt. Older conversation
attachments remain available through their download references.

The native XpressClaw MCP now exposes `create_task`. It defaults to the current
task as parent; explicit unfinished child tasks block that parent from
completing. An assignee or parent must belong to the calling Agent's Project.

## Share an interactive terminal

Open **Terminal** in Files. Enter a tmux session name and choose **Join /
create**, or select an existing session. Closing the terminal detaches the
client; the shell and interactive login prompt remain in the retained
container. Multiple clients can join the same session. **Share** copies a link
that opens Files with that session selected.

An Agent can create the same session using `tmux new-session -s login` or start
one detached with `tmux new-session -d -s login`. The user selects `login` in
XpressClaw and can interact with the same shell, including password prompts.
Sessions survive browser disconnects, not a container stop or replacement.

## Validate container access during development

CI exercises the Node helpers and the retained-container integration test
without an LLM or API key. To run the same test locally with Docker (or replace
`docker` with `podman`):

```bash
node --test harnesses/native/common/*.test.mjs
docker build -f crates/xpressclaw-core/tests/fixtures/environment.Dockerfile -t xpressclaw-environment-test:local crates/xpressclaw-core/tests/fixtures
XPRESSCLAW_ENVIRONMENT_TEST_IMAGE=xpressclaw-environment-test:local cargo test -p xpressclaw-core docker::environment::tests::retained_environment_roundtrip -- --ignored --exact --nocapture
```

The test uses an isolated installation and no host directory mounts. It checks
binary TCP transfers in both directions, port conflicts and removal, file and
folder access, tmux reconnection, and restoration after a container restart.
Without the explicit image variable, general `--ignored` runs skip this test.
