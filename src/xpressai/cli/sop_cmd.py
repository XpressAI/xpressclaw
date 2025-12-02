"""XpressAI SOP commands - Manage Standard Operating Procedures."""

from pathlib import Path
import click

from xpressai.tasks.sop import SOP, SOPInput, SOPOutput, SOPStep, SOPManager


def list_sops(sops_dir: Path | None = None) -> None:
    """List all SOPs."""
    manager = SOPManager(sops_dir)
    sops = manager.list_sops()

    if not sops:
        click.echo("No SOPs found.")
        click.echo(f"Create SOPs in: {manager.sops_dir}")
        return

    click.echo(click.style("Standard Operating Procedures", fg="cyan", bold=True))
    click.echo()

    for sop in sops:
        click.echo(click.style(f"  {sop.name}", fg="green", bold=True))
        if sop.summary:
            click.echo(f"    {sop.summary}")
        if sop.tools:
            click.echo(f"    Tools: {', '.join(sop.tools)}")
        click.echo()


def show_sop(name: str, sops_dir: Path | None = None) -> None:
    """Show details of an SOP."""
    manager = SOPManager(sops_dir)
    sop = manager.get(name)

    if not sop:
        click.echo(click.style(f"SOP not found: {name}", fg="red"))
        return

    click.echo(click.style(f"SOP: {sop.name}", fg="cyan", bold=True))
    click.echo()

    if sop.summary:
        click.echo(f"Summary: {sop.summary}")
        click.echo()

    if sop.tools:
        click.echo(click.style("Tools:", bold=True))
        for tool in sop.tools:
            click.echo(f"  - {tool}")
        click.echo()

    if sop.inputs:
        click.echo(click.style("Inputs:", bold=True))
        for inp in sop.inputs:
            default = f" (default: {inp.default})" if inp.default else ""
            click.echo(f"  - {inp.name}: {inp.context}{default}")
        click.echo()

    if sop.outputs:
        click.echo(click.style("Outputs:", bold=True))
        for out in sop.outputs:
            click.echo(f"  - {out.name}: {out.context}")
        click.echo()

    if sop.steps:
        click.echo(click.style("Steps:", bold=True))
        for i, step in enumerate(sop.steps, 1):
            click.echo(f"  {i}. {step.prompt}")
            if step.tools:
                click.echo(f"     Tools: {', '.join(step.tools)}")
            if step.inputs:
                click.echo(f"     Inputs: {', '.join(step.inputs)}")


def create_sop(name: str, sops_dir: Path | None = None) -> None:
    """Create a new SOP from a template."""
    manager = SOPManager(sops_dir)

    # Check if already exists
    if manager.get(name):
        click.echo(click.style(f"SOP already exists: {name}", fg="red"))
        return

    # Create a full example SOP
    sop = SOP(
        name=name,
        summary="Gets the current time and writes it to hello.txt",
        tools=["get_current_time", "write_file"],
        inputs=[
            SOPInput(
                name="output_path",
                context="The file path where the message will be written.",
                default="hello.txt",
            ),
        ],
        outputs=[
            SOPOutput(
                name="status",
                context="SUCCESS if the file was written, FAIL otherwise.",
            ),
        ],
        steps=[
            SOPStep(
                prompt="Get the current time.",
                tools=["get_current_time"],
                inputs=[],
            ),
            SOPStep(
                prompt='Write a file with the message "The current time is: {current_time}"',
                tools=["write_file"],
                inputs=["output_path"],
            ),
        ],
    )

    path = manager.create(sop)
    click.echo(click.style(f"Created SOP: {path}", fg="green"))
    click.echo("Edit the file to customize your workflow.")


def delete_sop(name: str, sops_dir: Path | None = None) -> None:
    """Delete an SOP."""
    manager = SOPManager(sops_dir)

    if manager.delete(name):
        click.echo(click.style(f"Deleted SOP: {name}", fg="green"))
    else:
        click.echo(click.style(f"SOP not found: {name}", fg="red"))
