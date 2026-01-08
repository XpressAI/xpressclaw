"""Procedure (SOP) management routes.

Contains page routes, API routes, and HTMX partials for Standard Operating Procedures.
"""

from __future__ import annotations

import html as html_module
import json
import logging
from typing import Optional

from fastapi import APIRouter, Request, HTTPException
from fastapi.responses import HTMLResponse

from xpressai.web.deps import get_runtime, get_templates

logger = logging.getLogger(__name__)

router = APIRouter()


# -------------------------
# Helper Functions
# -------------------------

def _format_sop_description(sop, input_values: dict[str, str]) -> str:
    """Format SOP into a markdown task description with input values substituted."""
    lines = [
        f"## Procedure: {sop.name}",
        "",
        sop.summary,
        "",
    ]

    if input_values:
        lines.append("### Inputs")
        lines.append("| Name | Value |")
        lines.append("|------|-------|")
        for name, value in input_values.items():
            lines.append(f"| {name} | {value} |")
        lines.append("")

    if sop.steps:
        lines.append("### Steps")
        for i, step in enumerate(sop.steps, 1):
            prompt = step.prompt
            # Substitute input values into prompts
            for inp_name, inp_value in input_values.items():
                prompt = prompt.replace(f"{{{inp_name}}}", inp_value)
            lines.append(f"{i}. {prompt}")
            if step.tools:
                lines.append(f"   - Tools: {', '.join(step.tools)}")
            if step.inputs:
                for inp in step.inputs:
                    inp_val = input_values.get(inp, "(not provided)")
                    lines.append(f"   - Uses input: {inp} = {inp_val}")
        lines.append("")

    if sop.outputs:
        lines.append("### Expected Outputs")
        for out in sop.outputs:
            lines.append(f"- {out.name}: {out.context}")

    return "\n".join(lines)


# -------------------------
# Page Routes
# -------------------------

@router.get("/procedures", response_class=HTMLResponse)
async def procedures_page(request: Request):
    """Procedures (SOP) page."""
    runtime = get_runtime()
    templates = get_templates()

    agents = []
    if runtime:
        agents = await runtime.list_agents()
    if templates:
        return templates.TemplateResponse(
            "procedures.html", {"request": request, "active": "procedures", "agents": agents}
        )
    return HTMLResponse("<h1>Procedures - Templates not installed</h1>")


# -------------------------
# API Routes
# -------------------------

@router.get("/api/procedures")
async def list_procedures():
    """List all procedures."""
    from xpressai.tasks.sop import SOPManager

    manager = SOPManager()
    sops = manager.list_sops()
    return {
        "procedures": [
            {
                "name": sop.name,
                "summary": sop.summary,
                "input_count": len(sop.inputs),
                "step_count": len(sop.steps),
            }
            for sop in sops
        ]
    }


@router.get("/api/procedures/{name}")
async def get_procedure(name: str):
    """Get procedure details."""
    from xpressai.tasks.sop import SOPManager

    manager = SOPManager()
    sop = manager.get(name)
    if not sop:
        raise HTTPException(status_code=404, detail="Procedure not found")

    return {
        "name": sop.name,
        "summary": sop.summary,
        "tools": sop.tools,
        "inputs": [
            {"name": inp.name, "context": inp.context, "default": inp.default}
            for inp in sop.inputs
        ],
        "outputs": [
            {"name": out.name, "context": out.context}
            for out in sop.outputs
        ],
        "steps": [
            {
                "prompt": step.prompt,
                "tools": step.tools,
                "inputs": step.inputs,
            }
            for step in sop.steps
        ],
    }


@router.post("/api/procedures/{name}/run")
async def run_procedure(name: str, request: Request):
    """Create a task from a procedure."""
    from xpressai.tasks.sop import SOPManager

    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    manager = SOPManager()
    sop = manager.get(name)
    if not sop:
        raise HTTPException(status_code=404, detail="Procedure not found")

    # Get form data
    form = await request.form()
    agent_id_raw = form.get("agent_id")
    agent_id: str | None = str(agent_id_raw) if agent_id_raw and agent_id_raw != "" else None

    # Collect input values
    input_values: dict[str, str] = {}
    for inp in sop.inputs:
        value = form.get(inp.name)
        if value:
            input_values[inp.name] = str(value)
        elif inp.default:
            input_values[inp.name] = inp.default

    # Format task description
    description = _format_sop_description(sop, input_values)

    # Create task
    task = await runtime.task_board.create_task(
        title=f"SOP: {sop.name}",
        description=description,
        agent_id=agent_id,
    )

    return {"status": "ok", "task_id": task.id}


@router.post("/api/procedures")
async def create_procedure(request: Request):
    """Create a new procedure."""
    from xpressai.tasks.sop import SOPManager, SOP, SOPInput, SOPOutput, SOPStep

    form = await request.form()

    name = form.get("name")
    if not name:
        raise HTTPException(status_code=400, detail="Name is required")
    name = str(name).strip()

    summary = str(form.get("summary", "")).strip()
    tools_raw = str(form.get("tools", "")).strip()
    tools = [t.strip() for t in tools_raw.split(",") if t.strip()] if tools_raw else []

    # Parse inputs (JSON array or comma-separated)
    inputs_raw = str(form.get("inputs", "")).strip()
    inputs: list[SOPInput] = []
    if inputs_raw:
        try:
            inputs_data = json.loads(inputs_raw)
            for inp in inputs_data:
                inputs.append(SOPInput(
                    name=inp.get("name", ""),
                    context=inp.get("context", ""),
                    default=inp.get("default"),
                ))
        except json.JSONDecodeError:
            # Treat as simple comma-separated names
            for inp_name in inputs_raw.split(","):
                inp_name = inp_name.strip()
                if inp_name:
                    inputs.append(SOPInput(name=inp_name, context=""))

    # Parse outputs (JSON array or comma-separated)
    outputs_raw = str(form.get("outputs", "")).strip()
    outputs: list[SOPOutput] = []
    if outputs_raw:
        try:
            outputs_data = json.loads(outputs_raw)
            for out in outputs_data:
                outputs.append(SOPOutput(
                    name=out.get("name", ""),
                    context=out.get("context", ""),
                ))
        except json.JSONDecodeError:
            # Treat as simple comma-separated names
            for out_name in outputs_raw.split(","):
                out_name = out_name.strip()
                if out_name:
                    outputs.append(SOPOutput(name=out_name, context=""))

    # Parse steps (JSON array)
    steps_raw = str(form.get("steps", "")).strip()
    steps: list[SOPStep] = []
    if steps_raw:
        try:
            steps_data = json.loads(steps_raw)
            for step in steps_data:
                step_tools = step.get("tools", [])
                if isinstance(step_tools, str):
                    step_tools = [t.strip() for t in step_tools.split(",") if t.strip()]
                step_inputs = step.get("inputs", [])
                if isinstance(step_inputs, str):
                    step_inputs = [i.strip() for i in step_inputs.split(",") if i.strip()]
                steps.append(SOPStep(
                    prompt=step.get("prompt", ""),
                    tools=step_tools,
                    inputs=step_inputs,
                ))
        except json.JSONDecodeError:
            raise HTTPException(status_code=400, detail="Invalid steps JSON format")

    # Create SOP
    sop = SOP(
        name=name,
        summary=summary,
        tools=tools,
        inputs=inputs,
        outputs=outputs,
        steps=steps,
    )

    manager = SOPManager()
    try:
        path = manager.create(sop)
        return {"status": "ok", "name": sop.name, "path": str(path)}
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to create procedure: {e}")


@router.put("/api/procedures/{name}")
async def update_procedure(name: str, request: Request):
    """Update an existing procedure."""
    from xpressai.tasks.sop import SOPManager, SOP, SOPInput, SOPOutput, SOPStep

    manager = SOPManager()

    # Check if procedure exists
    existing = manager.get(name)
    if not existing:
        raise HTTPException(status_code=404, detail="Procedure not found")

    form = await request.form()

    new_name = form.get("name")
    if not new_name:
        raise HTTPException(status_code=400, detail="Name is required")
    new_name = str(new_name).strip()

    summary = str(form.get("summary", "")).strip()
    tools_raw = str(form.get("tools", "")).strip()
    tools = [t.strip() for t in tools_raw.split(",") if t.strip()] if tools_raw else []

    # Parse inputs
    inputs_raw = str(form.get("inputs", "")).strip()
    inputs: list[SOPInput] = []
    if inputs_raw:
        try:
            inputs_data = json.loads(inputs_raw)
            for inp in inputs_data:
                inputs.append(SOPInput(
                    name=inp.get("name", ""),
                    context=inp.get("context", ""),
                    default=inp.get("default"),
                ))
        except json.JSONDecodeError:
            for inp_name in inputs_raw.split(","):
                inp_name = inp_name.strip()
                if inp_name:
                    inputs.append(SOPInput(name=inp_name, context=""))

    # Parse outputs
    outputs_raw = str(form.get("outputs", "")).strip()
    outputs: list[SOPOutput] = []
    if outputs_raw:
        try:
            outputs_data = json.loads(outputs_raw)
            for out in outputs_data:
                outputs.append(SOPOutput(
                    name=out.get("name", ""),
                    context=out.get("context", ""),
                ))
        except json.JSONDecodeError:
            for out_name in outputs_raw.split(","):
                out_name = out_name.strip()
                if out_name:
                    outputs.append(SOPOutput(name=out_name, context=""))

    # Parse steps
    steps_raw = str(form.get("steps", "")).strip()
    steps: list[SOPStep] = []
    if steps_raw:
        try:
            steps_data = json.loads(steps_raw)
            for step in steps_data:
                step_tools = step.get("tools", [])
                if isinstance(step_tools, str):
                    step_tools = [t.strip() for t in step_tools.split(",") if t.strip()]
                step_inputs = step.get("inputs", [])
                if isinstance(step_inputs, str):
                    step_inputs = [i.strip() for i in step_inputs.split(",") if i.strip()]
                steps.append(SOPStep(
                    prompt=step.get("prompt", ""),
                    tools=step_tools,
                    inputs=step_inputs,
                ))
        except json.JSONDecodeError:
            raise HTTPException(status_code=400, detail="Invalid steps JSON format")

    # Create updated SOP
    sop = SOP(
        name=new_name,
        summary=summary,
        tools=tools,
        inputs=inputs,
        outputs=outputs,
        steps=steps,
    )

    try:
        # Delete old and create new (handles name changes)
        manager.delete(name)
        path = manager.create(sop)
        return {"status": "ok", "name": sop.name, "path": str(path)}
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to update procedure: {e}")


@router.delete("/api/procedures/{name}")
async def delete_procedure(name: str):
    """Delete a procedure."""
    from xpressai.tasks.sop import SOPManager

    manager = SOPManager()

    # Check if procedure exists
    existing = manager.get(name)
    if not existing:
        raise HTTPException(status_code=404, detail="Procedure not found")

    if manager.delete(name):
        return {"status": "ok", "deleted": name}
    else:
        raise HTTPException(status_code=500, detail="Failed to delete procedure")


# -------------------------
# HTMX Partials
# -------------------------

@router.get("/partials/procedures/list", response_class=HTMLResponse)
async def procedures_list_partial(request: Request):
    """HTMX partial for procedures list."""
    from xpressai.tasks.sop import SOPManager

    manager = SOPManager()
    sops = manager.list_sops()

    if not sops:
        return HTMLResponse('<div class="empty-state">No procedures found</div>')

    html_parts = []
    for sop in sops:
        safe_name = html_module.escape(sop.name)
        safe_summary = html_module.escape(sop.summary or "(no summary)")[:60]
        input_count = len(sop.inputs)
        step_count = len(sop.steps)

        html_parts.append(f"""
            <div class="procedure-item" data-name="{safe_name}"
                 hx-get="/partials/procedures/{sop.name}"
                 hx-target="#procedure-detail"
                 hx-swap="innerHTML"
                 onclick="selectProcedure(this)">
                <div class="procedure-item-name">{safe_name}</div>
                <div class="procedure-item-summary">{safe_summary}</div>
                <div class="procedure-item-meta">
                    <span>{input_count} input{"s" if input_count != 1 else ""}</span>
                    <span>{step_count} step{"s" if step_count != 1 else ""}</span>
                </div>
            </div>
        """)

    return HTMLResponse("".join(html_parts))


@router.get("/partials/procedures/create-form", response_class=HTMLResponse)
async def procedure_create_form_partial(request: Request):
    """HTMX partial for procedure create form."""
    runtime = get_runtime()

    # Get agents for selection
    agents_options = '<option value="">Unassigned</option>'
    if runtime:
        agents = await runtime.list_agents()
        for agent in agents:
            escaped_name = html_module.escape(agent.name)
            agents_options += f'<option value="{escaped_name}">{escaped_name}</option>'

    return HTMLResponse(f"""
        <div class="procedure-form-container">
            <h3>Create New Procedure</h3>
            <form id="create-procedure-form" onsubmit="handleCreateProcedure(event)">
                <div class="form-field">
                    <label for="proc-name">Name *</label>
                    <input type="text" id="proc-name" name="name" required
                           placeholder="My Procedure">
                </div>
                <div class="form-field">
                    <label for="proc-summary">Summary</label>
                    <textarea id="proc-summary" name="summary" rows="2"
                              placeholder="Brief description of what this procedure does"></textarea>
                </div>
                <div class="form-field">
                    <label for="proc-tools">Tools (comma-separated)</label>
                    <input type="text" id="proc-tools" name="tools"
                           placeholder="read_file, write_file, execute_command">
                </div>

                <div class="form-section">
                    <h4>Inputs</h4>
                    <div id="inputs-container"></div>
                    <button type="button" class="btn-small" onclick="addInput()">+ Add Input</button>
                </div>

                <div class="form-section">
                    <h4>Steps</h4>
                    <div id="steps-container"></div>
                    <button type="button" class="btn-small" onclick="addStep()">+ Add Step</button>
                </div>

                <div class="form-section">
                    <h4>Outputs</h4>
                    <div id="outputs-container"></div>
                    <button type="button" class="btn-small" onclick="addOutput()">+ Add Output</button>
                </div>

                <div class="form-actions">
                    <button type="button" class="btn-secondary" onclick="closeCreateForm()">Cancel</button>
                    <button type="submit" class="btn-primary">Create Procedure</button>
                </div>
            </form>
        </div>
    """)


@router.get("/partials/procedures/{name}", response_class=HTMLResponse)
async def procedure_detail_partial(request: Request, name: str):
    """HTMX partial for procedure detail."""
    from xpressai.tasks.sop import SOPManager

    manager = SOPManager()
    sop = manager.get(name)

    if not sop:
        return HTMLResponse('<div class="empty-state">Procedure not found</div>')

    safe_name = html_module.escape(sop.name)
    safe_summary = html_module.escape(sop.summary or "(no summary)")

    # Tools section
    tools_html = ""
    if sop.tools:
        tools_html = '<div class="detail-section"><h4>Tools</h4><div class="tools-list">'
        for tool in sop.tools:
            tools_html += f'<span class="tool-tag">{html_module.escape(tool)}</span>'
        tools_html += '</div></div>'

    # Inputs section
    inputs_html = ""
    if sop.inputs:
        inputs_html = '<div class="detail-section"><h4>Inputs</h4><ul class="inputs-list">'
        for inp in sop.inputs:
            default_html = ""
            if inp.default:
                escaped_default = html_module.escape(inp.default)
                default_html = f'<div class="input-default">Default: {escaped_default}</div>'
            inputs_html += f"""
                <li class="input-item">
                    <div class="input-name">{html_module.escape(inp.name)}</div>
                    <div class="input-context">{html_module.escape(inp.context or "")}</div>
                    {default_html}
                </li>
            """
        inputs_html += '</ul></div>'

    # Steps section
    steps_html = ""
    if sop.steps:
        steps_html = '<div class="detail-section"><h4>Steps</h4><ol class="steps-list">'
        for step in sop.steps:
            step_tools = ""
            if step.tools:
                step_tools = '<div class="step-tools">Tools: ' + ", ".join(
                    html_module.escape(t) for t in step.tools
                ) + '</div>'
            step_inputs = ""
            if step.inputs:
                step_inputs = '<div class="step-inputs">Uses: ' + ", ".join(
                    html_module.escape(i) for i in step.inputs
                ) + '</div>'
            steps_html += f"""
                <li class="step-item">
                    <div class="step-prompt">{html_module.escape(step.prompt)}</div>
                    {step_tools}
                    {step_inputs}
                </li>
            """
        steps_html += '</ol></div>'

    # Outputs section
    outputs_html = ""
    if sop.outputs:
        outputs_html = '<div class="detail-section">'
        outputs_html += '<h4>Expected Outputs</h4><ul class="outputs-list">'
        for out in sop.outputs:
            outputs_html += f"""
                <li class="output-item">
                    <div class="output-name">{html_module.escape(out.name)}</div>
                    <div class="output-context">{html_module.escape(out.context or "")}</div>
                </li>
            """
        outputs_html += '</ul></div>'

    return HTMLResponse(f"""
        <div class="procedure-detail-header">
            <h3>{safe_name}</h3>
            <p class="procedure-summary">{safe_summary}</p>
        </div>
        {tools_html}
        {inputs_html}
        {steps_html}
        {outputs_html}
        <div class="procedure-actions">
            <button class="btn-primary"
                    hx-get="/partials/procedures/{name}/run-form"
                    hx-target="#run-form-container"
                    hx-swap="innerHTML">
                Run Procedure
            </button>
            <button class="btn-secondary"
                    hx-get="/partials/procedures/{name}/edit-form"
                    hx-target="#run-form-container"
                    hx-swap="innerHTML">
                Edit
            </button>
            <button class="btn-danger"
                    onclick="deleteProcedure('{name}')">
                Delete
            </button>
        </div>
        <div id="run-form-container"></div>
    """)


@router.get("/partials/procedures/{name}/run-form", response_class=HTMLResponse)
async def procedure_run_form_partial(request: Request, name: str):
    """HTMX partial for procedure run form."""
    from xpressai.tasks.sop import SOPManager

    runtime = get_runtime()
    manager = SOPManager()
    sop = manager.get(name)

    if not sop:
        return HTMLResponse('<div class="empty-state">Procedure not found</div>')

    # Build input fields
    inputs_html = ""
    if sop.inputs:
        for inp in sop.inputs:
            default_val = html_module.escape(inp.default or "") if inp.default else ""
            context_html = ""
            if inp.context:
                context_html = f'<p class="help-text">{html_module.escape(inp.context)}</p>'
            inputs_html += f"""
                <div class="form-field">
                    <label for="input_{inp.name}">{html_module.escape(inp.name)}</label>
                    {context_html}
                    <input type="text"
                           id="input_{inp.name}"
                           name="{html_module.escape(inp.name)}"
                           value="{default_val}"
                           placeholder="Enter value...">
                </div>
            """

    # Get agents for selection
    agents_options = '<option value="">Unassigned</option>'
    if runtime:
        agents = await runtime.list_agents()
        for agent in agents:
            escaped_name = html_module.escape(agent.name)
            agents_options += f'<option value="{escaped_name}">{escaped_name}</option>'

    safe_name = html_module.escape(sop.name)

    return HTMLResponse(f"""
        <div class="run-form-modal">
            <h4>Run: {safe_name}</h4>
            <form hx-post="/api/procedures/{name}/run"
                  hx-swap="none"
                  onsubmit="handleProcedureRun(event)">
                {inputs_html}
                <div class="form-field">
                    <label for="agent_id">Assign to Agent</label>
                    <select id="agent_id" name="agent_id">
                        {agents_options}
                    </select>
                </div>
                <div class="form-actions">
                    <button type="button" class="btn-secondary"
                            onclick="closeRunForm()">Cancel</button>
                    <button type="submit" class="btn-primary">Create Task</button>
                </div>
            </form>
        </div>
    """)


@router.get("/partials/procedures/{name}/edit-form", response_class=HTMLResponse)
async def procedure_edit_form_partial(request: Request, name: str):
    """HTMX partial for procedure edit form."""
    from xpressai.tasks.sop import SOPManager

    manager = SOPManager()
    sop = manager.get(name)

    if not sop:
        return HTMLResponse('<div class="empty-state">Procedure not found</div>')

    safe_name = html_module.escape(sop.name)
    safe_summary = html_module.escape(sop.summary or "")
    tools_str = html_module.escape(", ".join(sop.tools) if sop.tools else "")

    # Pre-populate inputs
    inputs_html = ""
    for i, inp in enumerate(sop.inputs):
        inp_name = html_module.escape(inp.name)
        inp_context = html_module.escape(inp.context or "")
        inp_default = html_module.escape(inp.default or "")
        inputs_html += f"""
            <div class="input-entry" data-index="{i}">
                <input type="text" placeholder="Name" value="{inp_name}" class="input-name">
                <input type="text" placeholder="Description" value="{inp_context}" class="input-context">
                <input type="text" placeholder="Default" value="{inp_default}" class="input-default">
                <button type="button" class="btn-remove" onclick="removeEntry(this)">×</button>
            </div>
        """

    # Pre-populate steps
    steps_html = ""
    for i, step in enumerate(sop.steps):
        step_prompt = html_module.escape(step.prompt)
        step_tools = html_module.escape(", ".join(step.tools) if step.tools else "")
        step_inputs = html_module.escape(", ".join(step.inputs) if step.inputs else "")
        steps_html += f"""
            <div class="step-entry" data-index="{i}">
                <textarea placeholder="Step prompt" class="step-prompt" rows="2">{step_prompt}</textarea>
                <input type="text" placeholder="Tools (comma-separated)" value="{step_tools}" class="step-tools">
                <input type="text" placeholder="Inputs used (comma-separated)" value="{step_inputs}" class="step-inputs">
                <button type="button" class="btn-remove" onclick="removeEntry(this)">×</button>
            </div>
        """

    # Pre-populate outputs
    outputs_html = ""
    for i, out in enumerate(sop.outputs):
        out_name = html_module.escape(out.name)
        out_context = html_module.escape(out.context or "")
        outputs_html += f"""
            <div class="output-entry" data-index="{i}">
                <input type="text" placeholder="Name" value="{out_name}" class="output-name">
                <input type="text" placeholder="Description" value="{out_context}" class="output-context">
                <button type="button" class="btn-remove" onclick="removeEntry(this)">×</button>
            </div>
        """

    return HTMLResponse(f"""
        <div class="procedure-form-container">
            <h3>Edit Procedure</h3>
            <form id="edit-procedure-form" onsubmit="handleEditProcedure(event, '{name}')">
                <div class="form-field">
                    <label for="proc-name">Name *</label>
                    <input type="text" id="proc-name" name="name" required
                           value="{safe_name}">
                </div>
                <div class="form-field">
                    <label for="proc-summary">Summary</label>
                    <textarea id="proc-summary" name="summary" rows="2">{safe_summary}</textarea>
                </div>
                <div class="form-field">
                    <label for="proc-tools">Tools (comma-separated)</label>
                    <input type="text" id="proc-tools" name="tools" value="{tools_str}">
                </div>

                <div class="form-section">
                    <h4>Inputs</h4>
                    <div id="inputs-container">{inputs_html}</div>
                    <button type="button" class="btn-small" onclick="addInput()">+ Add Input</button>
                </div>

                <div class="form-section">
                    <h4>Steps</h4>
                    <div id="steps-container">{steps_html}</div>
                    <button type="button" class="btn-small" onclick="addStep()">+ Add Step</button>
                </div>

                <div class="form-section">
                    <h4>Outputs</h4>
                    <div id="outputs-container">{outputs_html}</div>
                    <button type="button" class="btn-small" onclick="addOutput()">+ Add Output</button>
                </div>

                <div class="form-actions">
                    <button type="button" class="btn-secondary" onclick="closeRunForm()">Cancel</button>
                    <button type="submit" class="btn-primary">Save Changes</button>
                </div>
            </form>
        </div>
    """)
