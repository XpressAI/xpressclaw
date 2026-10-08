# Xpress AI Connect

Connect publishes selected XpressClaw Agents into your Xpress AI projects. People
can add them to platform chats or assign tasks. The work runs in the Agent's
existing local container, using its local workspace, harness and model settings.

Both XpressClaw and the platform need Connect protocol version 1 support.

## Connect an instance

1. In XpressClaw, open **Settings → Xpress AI Connect**. Enter your platform URL
   and a name for this instance, then choose **Connect account**.
2. Open the approval link, sign into Xpress AI, verify the displayed code, and
   select the projects this instance may access. Choose **Approve this instance**.
3. Return to XpressClaw and choose **I’ve approved the connection**.
4. Select a local Project and Agent, an approved platform project, and an unused
   platform Agent name. Choose **Publish Agent**.
5. In Xpress AI, add the published Agent to a chat or assign it a task normally.

Pair each installation separately. A local Project maps to one platform project.
Republishing a disconnected Agent into the same binding retains its platform
identity and advances its authorization generation. Harness and model changes
are made in XpressClaw.

## Working with connected Agents

Keep XpressClaw running. Offline Agents are shown as offline; queued work can
wait for the instance to reconnect. If an executing instance stops responding,
the execution lease expires and the platform pauses ambiguous task outcomes
instead of rerunning possible side effects automatically.

Platform tasks appear in the local Tasks view and execute as native task
attempts. Follow-ups and retries reuse the same local task. Platform chats reuse
a mapped Conversation rather than creating one for every message. Continue hosted
chats and tasks from Xpress AI. Permission prompts remain in the local
XpressClaw UI in this first version.

Agents publish files through their connected message/file tools. Limits are
8 MiB per file and 20 MiB per execution command. Platform messages contain
account-protected download links. Input chat uploads and published Connect files
are available through the attachment tools. Files mentioned only by a local path
in a final reply are not uploaded automatically.

Connected tasks support checklist updates, Done/Failed/Waiting/Doing, and
independent follow-up tasks assigned to the same Agent. The platform applies its
normal completion checks. Local wake-up scheduling, local delegation, local
memory-editing tools, cloud voice/desktop controls, and custom replacements for
the bundled control MCP are outside this first version.

## Disconnect

Use **Disconnect Agent** to retire one binding, or **Disconnect instance** to
stop all incoming platform work. You can also revoke an installation from
**Xpress AI Settings → Xpress AI Connect**. If local disconnect cannot reach the
platform, it still disables local authority and tells you to revoke remotely.
Revocation cannot undo work already performed; running work stops through the
existing interruption controls and bounded execution lease.

If pairing expires, start it again. If approval was collected but the response
was lost, revoke the unused entry in platform settings and pair again. Never
copy a live connection credential to another running installation.
