import type { Task } from "$lib/api";
import { serverTimestampMs } from "$lib/serverTime";
export const lanes = [
  { id: "backlog", label: "Backlog", color: "#78716c" },
  { id: "queue", label: "To do", color: "#64748b" },
  { id: "scheduled", label: "Scheduled", color: "#0369a1" },
  { id: "working", label: "Working", color: "#2563eb" },
  { id: "input", label: "Needs you", color: "#c2410c" },
  { id: "blocked", label: "Blocked", color: "#dc2626" },
  { id: "review", label: "Review", color: "#7c3aed" },
  { id: "done", label: "Done", color: "#047857" },
];
export function taskLane(task: Task): string {
  return (
    task.planning?.lane ??
    {
      pending: "queue",
      in_progress: "working",
      waiting_for_input: "input",
      blocked: "blocked",
      completed: "done",
      cancelled: "done",
      scheduled: "scheduled",
      backlog: "backlog",
      awaiting_review: "review",
    }[task.activity_status ?? task.status] ??
    "queue"
  );
}
export function taskLabel(task: Task): string {
  if (task.status === "cancelled") return "Cancelled";
  if (taskLane(task) === "queue" && task.planning && !task.planning.queued)
    return task.status === "in_progress" ? "Not running" : task.agent_id ? "To do · not queued" : "To do · unassigned";
  return lanes.find((l) => l.id === taskLane(task))?.label ?? task.status;
}
export function localInput(value: string | number | null | undefined): string {
  const ms = serverTimestampMs(value);
  if (ms === null) return "";
  const date = new Date(ms);
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}T${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}
export function scheduleTimestamp(value: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  if (!Number.isFinite(date.getTime()) || localInput(date.getTime()) !== value)
    throw new Error(
      "Choose a valid local date and time. This time may fall in a daylight-saving clock change.",
    );
  return date.toISOString();
}
export function dateLabel(value: string | number | null | undefined): string {
  const ms = serverTimestampMs(value);
  return ms === null
    ? "Unscheduled"
    : new Date(ms).toLocaleString(undefined, {
        month: "short",
        day: "numeric",
        year: "numeric",
        hour: "numeric",
        minute: "2-digit",
        timeZoneName: "short",
      });
}
export function timezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

// Keep modal controls above the software keyboard, including from task details.
export function planningViewport(node: HTMLDialogElement) {
  const viewport = window.visualViewport;
  const update = () => {
    node.style.setProperty('--planning-viewport-height', String(viewport?.height ?? innerHeight) + 'px');
    node.style.setProperty('--planning-viewport-top', String(viewport?.offsetTop ?? 0) + 'px');
  };
  update();
  viewport?.addEventListener('resize', update);
  viewport?.addEventListener('scroll', update);
  return { destroy() { viewport?.removeEventListener('resize', update); viewport?.removeEventListener('scroll', update); } };
}
