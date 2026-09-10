import { expect, test, type Page } from "@playwright/test";

async function planningApi(page: Page) {
  const now = new Date();
  now.setHours(9, 0, 0, 0);
  const stamp = now.toISOString();
  const projectNames = ["Commerce", "Research", "Operations"];
  const projects = projectNames.map((name, i) => ({
    id: `project-${i}`,
    name,
    description: null,
    created_at: stamp,
    updated_at: stamp,
    agent_count: 2,
    conversation_count: 0,
    task_count: 20,
    icon: null,
  }));
  const agents = projects.flatMap((project, i) =>
    [0, 1].map((n) => ({
      id: `agent-${i}-${n}`,
      name: `agent-${i}-${n}`,
      title: ["Atlas", "Nova"][n],
      project_id: project.id,
      status: "idle",
      backend: "native",
      workspace: "/workspace",
      runner: { kind: "codex" },
      tools: [],
      model: "codex",
    })),
  );
  const lanes = [
    "queue",
    "scheduled",
    "working",
    "input",
    "blocked",
    "backlog",
    "review",
    "done",
  ];
  let rows = Array.from({ length: 64 }, (_, i) => {
    const lane = lanes[i % lanes.length];
    const date = new Date(now);
    date.setDate(date.getDate() + 2 + Math.floor(i / 8));
    return {
      id: `task-${i}`,
      title:
        i === 0
          ? "Plan API rollout"
          : i === 8
            ? "Prepare release"
            : `${["Audit checkout flow", "Review experiment results", "Update runbook", "Investigate agent output", "Integrate payment API", "Document release process", "Review pull request", "Improve onboarding"][i % 8]} ${i + 1}`,
      description: null,
      status:
        lane === "done"
          ? "completed"
          : lane === "working"
            ? "in_progress"
            : lane === "input"
              ? "waiting_for_input"
              : lane === "blocked"
                ? "blocked"
                : "pending",
      priority: 0,
      position: i * 1024,
      revision: 0,
      agent_id: `agent-${i % 3}-${i % 2}`,
      project_id: `project-${i % 3}`,
      parent_task_id: null,
      sop_id: null,
      conversation_id: null,
      created_at: stamp,
      updated_at: stamp,
      completed_at: lane === "done" ? stamp : null,
      context: null,
      start_after: lane === "scheduled" ? date.toISOString() : null,
      backlog: lane === "backlog",
      depends_on: lane === "blocked" ? ["task-0"] : [],
      planning: {
        lane,
        disabled_reason: [
          "working",
          "input",
          "blocked",
          "review",
          "done",
        ].includes(lane)
          ? "This task is controlled by its current lifecycle. Open task details."
          : null,
        queued: ["queue", "scheduled", "backlog", "blocked"].includes(lane),
        actual_started_at: lane === "working" || lane === "done" ? stamp : null,
      },
    };
  });
  const changes: Record<string, unknown>[] = [];
  const creates: Record<string, unknown>[] = [];
  await page.addInitScript(() => {
    localStorage.removeItem("xpressclaw.workspace.v1");
    localStorage.removeItem("xpressclaw.tasks.view");
  });
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    let data: unknown = [];
    if (path === "/api/health") data = { status: "ok", version: "0.2.0" };
    else if (path === "/api/setup/status") data = { setup_complete: true };
    else if (path === "/api/setup/check-docker") data = { available: true };
    else if (path === "/api/projects") data = projects;
    else if (path === "/api/agents") data = agents;
    else if (path === "/api/settings/sync") data = { projects: [] };
    else if (path === "/api/tasks/planning") {
      const p = url.searchParams;
      const status = p.get("status") ?? "active";
      const search = (p.get("search") ?? "").toLowerCase();
      const matches = rows.filter(
        (t) =>
          (!p.get("project_id") || t.project_id === p.get("project_id")) &&
          (!p.get("agent_id") || t.agent_id === p.get("agent_id")) &&
          t.title.toLowerCase().includes(search),
      );
      const counts: Record<string, number> = {};
      for (const row of matches)
        counts[row.planning.lane] = (counts[row.planning.lane] ?? 0) + 1;
      const filtered = matches
        .filter(
          (t) =>
            status === "all" ||
            (status === "active" && t.planning.lane !== "done") ||
            (status === "attention" &&
              ["input", "blocked", "review"].includes(t.planning.lane)) ||
            status === t.planning.lane,
        )
        .sort((a, b) => b.priority - a.priority || a.position - b.position);
      const offset = Number(p.get("offset") ?? 0);
      data = {
        tasks: filtered.slice(offset, offset + Number(p.get("limit") ?? 100)),
        total: filtered.length,
        counts,
      };
    } else if (/^\/api\/tasks\/[^/]+\/planning$/.test(path)) {
      const row = rows.find((t) => t.id === path.split("/")[3])!;
      if (request.method() === "PATCH") {
        const change = request.postDataJSON();
        changes.push(change);
        if (
          change.expected_revision !== row.revision ||
          row.planning.disabled_reason
        ) {
          await route.fulfill({
            status: 409,
            json: {
              error: "Task changed since it was loaded. Refresh and try again.",
            },
          });
          return;
        }
        if (change.action === "backlog") {
          row.backlog = true;
          row.planning.lane = "backlog";
        }
        if (change.action === "queue") {
          row.backlog = false;
          row.planning.lane = row.start_after ? "scheduled" : "queue";
        }
        if (change.action === "schedule") {
          row.start_after = change.start_after;
          if (change.activate) row.backlog = false;
          row.planning.lane = row.backlog
            ? "backlog"
            : change.start_after
              ? "scheduled"
              : "queue";
        }
        if (change.action === "priority") row.priority = change.priority;
        if (change.action === "assign") row.agent_id = change.agent_id;
        if (change.action === "reorder") {
          const target = rows.find(
            (t) => t.id === (change.before_id ?? change.after_id),
          )!;
          row.position = target.position + (change.before_id ? -1 : 1);
          row.priority = target.priority;
        }
        row.revision++;
        row.updated_at = new Date().toISOString();
      }
      data = row;
    } else if (path === "/api/tasks/batch") {
      const payload = request.postDataJSON();
      creates.push(payload);
      data = payload.tasks;
    } else if (path === "/api/tasks" || path === "/api/tasks/recent-by-agent")
      data = {
        tasks: rows.slice(0, 20),
        counts: {
          pending: 48,
          in_progress: 8,
          waiting_for_input: 8,
          blocked: 8,
          completed: 8,
          cancelled: 0,
        },
      };
    else if (path === "/api/settings") data = {};
    else if (/^\/api\/agents\/[^/]+$/.test(path))
      data = agents.find((a) => a.id === path.split("/")[3]);
    else if (/^\/api\/projects\/[^/]+$/.test(path))
      data = projects.find((p) => p.id === path.split("/")[3]);
    await route.fulfill({ json: data });
  });
  return {
    changes,
    creates,
    rows,
    concurrentClaim: () => {
      rows[0].revision++;
      rows[0].planning.lane = "working";
      rows[0].planning.disabled_reason = "This task has an active turn.";
    },
  };
}

test("backlog has a quick collection flow while ordinary creation defaults to To do", async ({ page }) => {
  const state = await planningApi(page);
  await page.goto("/tasks?view=board");
  await page.getByRole("button", { name: "+ Add to Backlog", exact: true }).click();
  const create = page.getByRole("dialog", { name: "Create task", exact: true });
  await expect(create.getByLabel("Start in")).toHaveValue("true");
  await create.getByLabel("Task title").fill("Explore next quarter ideas");
  await create.getByRole("button", { name: "Add to Backlog", exact: true }).click();
  await expect.poll(() => state.creates.length).toBe(1);
  expect((state.creates[0].tasks as Record<string, unknown>[])[0].backlog).toBe(true);
  await page.getByRole("button", { name: "New Task", exact: true }).click();
  await expect(create.getByLabel("Start in")).toHaveValue("false");
  await create.getByLabel("Task title").fill("Implement a ready task");
  await create.getByRole("button", { name: "Create in To do", exact: true }).click();
  await expect.poll(() => state.creates.length).toBe(2);
  expect((state.creates[1].tasks as Record<string, unknown>[])[0].backlog).toBe(false);
});

test("planning board supports desktop moves, persisted order, and concurrent updates", async ({
  page,
}, info) => {
  await page.setViewportSize({ width: 2400, height: 1000 });
  const state = await planningApi(page);
  await page.goto("/tasks?view=board");
  const card = page.locator('[data-planning-card="task-0"]');
  await expect(card).toContainText("Commerce");
  await expect(card).toContainText("Atlas");
  await card.dragTo(page.locator('[data-planning-card="task-8"]'));
  await expect.poll(() => state.changes.at(-1)?.action).toBe("reorder");
  await expect(page.locator(".task-planner")).toHaveAttribute(
    "aria-busy",
    "false",
  );
  await card.dragTo(page.locator('[data-planning-card="task-5"]'));
  await expect(
    page.locator('[data-board-lane="backlog"] [data-planning-card="task-0"]'),
  ).toBeVisible();
  expect(state.rows[0].status).toBe("pending");
  await card
    .getByRole("button", { name: "Actions for Plan API rollout" })
    .click();
  const sheet = page.getByRole("dialog", { name: "Plan Plan API rollout" });
  await sheet
    .getByRole("combobox", { name: "Move task to" })
    .selectOption("queue");
  await sheet.getByRole("button", { name: "Move task", exact: true }).click();
  await expect.poll(() => state.rows[0].backlog).toBe(false);
  await sheet.getByRole("button", { name: "Done", exact: true }).click();
  await card.getByRole("button").focus();
  await page.keyboard.press("Alt+ArrowDown");
  await expect.poll(() => state.changes.at(-1)?.action).toBe("reorder");
  await Promise.all([
    page.waitForResponse(
      (r) =>
        r.url().endsWith("/api/tasks/task-0/planning") &&
        r.request().method() === "GET",
    ),
    card.getByRole("button").click(),
  ]);
  state.concurrentClaim();
  await sheet.getByLabel("Start no earlier than").fill("2030-10-12T09:30");
  await sheet.getByRole("button", { name: "Save schedule" }).click();
  await expect(sheet.getByRole("alert")).toContainText("Task changed");
  expect(state.rows[0].start_after).toBeNull();
  await sheet.getByRole("button", { name: "Close task actions" }).click();
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator("[data-task-board]").evaluate((el) => el.scrollTo(0, 0));
  await page.screenshot({
    path: info.outputPath("planning-board-desktop.png"),
  });
});

test("planning timeline moves start markers and shows dependencies without completion predictions", async ({
  page,
}, info) => {
  const state = await planningApi(page);
  await page.goto("/tasks?view=timeline");
  const row = page.locator('[data-timeline-task="task-1"]');
  const marker = row.getByRole("button", { name: /Earliest start for/ });
  const target = row.locator("[data-schedule-day]").nth(4);
  const day = await target.getAttribute("data-schedule-day");
  await marker.dragTo(target);
  await expect.poll(() => state.changes.at(-1)?.action).toBe("schedule");
  expect(state.rows[1].start_after?.slice(0, 10)).toBe(day);
  await expect(
    page.getByText("Completion times are not predicted."),
  ).toBeVisible();
  await expect(page.locator('[data-timeline-task="task-4"]')).toContainText(
    "Depends on Plan API rollout",
  );
  await expect(
    page.getByText("Unscheduled · choose a date").first(),
  ).toBeVisible();
  await page
    .locator("[data-planning-timeline]")
    .evaluate((el) => el.scrollTo(0, 0));
  await page.screenshot({
    path: info.outputPath("planning-timeline-desktop.png"),
  });
});

test("planning phone controls support filtering, assignment, priority, schedule, backlog, and creation with taps", async ({
  browser,
}, info) => {
  const page = await browser.newPage({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    timezoneId: "Asia/Tokyo",
  });
  const state = await planningApi(page);
  await page.goto("/tasks");
  const controls = page.getByRole("group", { name: "Task controls" });
  await controls.getByRole("button", { name: "Board", exact: true }).tap();
  await expect(page.getByRole("combobox", { name: "Board lane" })).toHaveValue(
    "queue",
  );
  const box = await controls.boundingBox();
  expect(box!.y).toBeGreaterThan(700);
  await page.screenshot({ path: info.outputPath("planning-board-phone.png") });
  await page
    .getByRole("button", { name: "Actions for Plan API rollout" })
    .tap();
  const sheet = page.getByRole("dialog", { name: "Plan Plan API rollout" });
  await sheet
    .getByRole("combobox", { name: "Assigned agent" })
    .selectOption("agent-0-1");
  expect(await sheet.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  expect(await sheet.locator(".sheet-content").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath("planning-actions-phone.png") });
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { value: 460, configurable: true });
    visualViewport?.dispatchEvent(new Event("resize"));
  });
  await expect.poll(async () => { const box = await sheet.boundingBox(); return Math.round(box!.y + box!.height); }).toBeLessThanOrEqual(460);
  await expect(sheet.getByRole("button", { name: "Done", exact: true })).toBeVisible();
  await page.evaluate(() => { Reflect.deleteProperty(visualViewport!, "height"); visualViewport?.dispatchEvent(new Event("resize")); });
  await sheet.getByRole("button", { name: "Move down", exact: false }).tap();
  await expect.poll(() => state.changes.at(-1)?.action).toBe("reorder");
  await sheet.getByRole("combobox", { name: "Assigned agent" }).selectOption("agent-0-1");
  await sheet.getByRole("button", { name: "Assign", exact: true }).tap();
  await sheet
    .getByRole("combobox", { name: "Task priority" })
    .selectOption("10");
  await sheet.getByRole("button", { name: "Set priority" }).tap();
  await sheet.getByLabel("Start no earlier than").fill("2030-10-12T09:30");
  await sheet.getByRole("button", { name: "Save schedule" }).tap();
  await expect
    .poll(() => state.rows[0].start_after)
    .toBe("2030-10-12T00:30:00.000Z");
  await sheet.getByRole("combobox", { name: "Move task to" }).selectOption("backlog");
  await sheet.getByRole("button", { name: "Move task", exact: true }).tap();
  await sheet.getByRole("combobox", { name: "Move task to" }).selectOption("queue");
  await sheet.getByRole("button", { name: "Move task", exact: true }).tap();
  await expect.poll(() => state.rows[0].backlog).toBe(false);
  expect(state.rows[0].start_after).toBe("2030-10-12T00:30:00.000Z");
  await sheet.getByRole("button", { name: "Clear schedule" }).tap();
  await sheet
    .getByRole("combobox", { name: "Move task to" })
    .selectOption("backlog");
  await sheet.getByRole("button", { name: "Move task", exact: true }).tap();
  await sheet.getByRole("button", { name: "Done", exact: true }).tap();
  await controls.getByRole("button", { name: "Filters", exact: true }).tap();
  const filters = page.getByRole("dialog", { name: "Task filters" });
  await filters
    .getByRole("combobox", { name: "Filter project" })
    .selectOption("project-1");
  await filters
    .getByRole("combobox", { name: "Filter status" })
    .selectOption("active");
  await filters.getByRole("button", { name: "Apply filters" }).tap();
  await controls.getByRole("button", { name: "Timeline", exact: true }).tap();
  await expect(page.locator("[data-planning-agenda]")).toBeVisible();
  await expect(page.locator("[data-planning-timeline]")).toBeHidden();
  await expect(page.locator("[data-planning-agenda]")).toContainText(
    "Research",
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({ path: info.outputPath("planning-agenda-phone.png") });
  await controls.getByRole("button", { name: "+ Task", exact: true }).tap();
  const create = page.getByRole("dialog", { name: "Create task", exact: true });
  await create.getByLabel("Task title").fill("Schedule overnight integration");
  await create.getByLabel("Start no earlier than").fill("2030-10-13T09:00");
  await create.getByRole("button", { name: "Create scheduled task" }).tap();
  await expect.poll(() => state.creates.length).toBe(1);
  expect((state.creates[0].tasks as Record<string, unknown>[])[0].backlog).toBe(false);
  expect(
    (state.creates[0].tasks as Record<string, unknown>[])[0].start_after,
  ).toBe("2030-10-13T00:00:00.000Z");
  await page.close();
});
