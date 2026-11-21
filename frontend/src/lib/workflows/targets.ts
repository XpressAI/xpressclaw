export interface WorkflowStepTarget {
	id: string;
	label: string;
	number: string;
}

export function parseWorkflowTarget(target: string, currentFlow: string): { flow: string; step?: string } | null {
	const step = /^step\s+(\S+)$/.exec(target.trim());
	if (step) return { flow: currentFlow, step: step[1] };
	const flow = /^flow\s+(\S+)(?:\s+step\s+(\S+))?$/.exec(target.trim());
	return flow ? { flow: flow[1], step: flow[2] } : null;
}
