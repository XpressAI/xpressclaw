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

// Keep modal controls above the keyboard and inside their pane's fold segment.
export function planningViewport(node: HTMLDialogElement) {
  const viewport = window.visualViewport;
  const pane = node.closest<HTMLElement>('[data-workspace-pane]');
  const properties = ['position', 'inset', 'left', 'right', 'top', 'bottom', 'width', 'max-height', 'margin', 'transform'];
  const original = properties.map(property => node.style.getPropertyValue(property));
  let constrained = false;
  let frame = 0;
  const restore = () => {
    properties.forEach((property, i) => {
      if (original[i]) node.style.setProperty(property, original[i]);
      else node.style.removeProperty(property);
    });
    constrained = false;
  };
  const update = () => {
    frame = 0;
    node.style.setProperty('--planning-viewport-height', String(viewport?.height ?? innerHeight) + 'px');
    node.style.setProperty('--planning-viewport-top', String(viewport?.offsetTop ?? 0) + 'px');
    const bounds = pane?.getBoundingClientRect();
    if (bounds?.width && pane?.closest('[data-foldable]')) {
      constrained = true;
      node.style.position = 'fixed';
      node.style.inset = 'auto';
      node.style.left = (bounds.x + 8) + 'px';
      node.style.top = (bounds.y + 8) + 'px';
      node.style.width = Math.max(0, bounds.width - 16) + 'px';
      node.style.maxHeight = Math.max(0, bounds.height - 16) + 'px';
      node.style.margin = '0';
      node.style.transform = 'none';
    } else if (constrained) restore();
  };
  const scheduleUpdate = () => { if (!frame) frame = requestAnimationFrame(update); };
  const resize = new ResizeObserver(scheduleUpdate);
  const position = new MutationObserver(scheduleUpdate);
  if (pane) {
    resize.observe(pane);
    position.observe(pane, { attributes: true, attributeFilter: ['style'] });
  }
  update();
  viewport?.addEventListener('resize', scheduleUpdate);
  viewport?.addEventListener('scroll', scheduleUpdate);
  window.addEventListener('resize', scheduleUpdate);
  return { destroy() {
    cancelAnimationFrame(frame);
    resize.disconnect();
    position.disconnect();
    viewport?.removeEventListener('resize', scheduleUpdate);
    viewport?.removeEventListener('scroll', scheduleUpdate);
    window.removeEventListener('resize', scheduleUpdate);
    if (constrained) restore();
  } };
}
