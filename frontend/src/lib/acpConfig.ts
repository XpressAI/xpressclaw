import type { AcpConfigOption } from './api';

export function selectChoices(option: AcpConfigOption): { value: string; name: string; description?: string | null }[] {
	if (!Array.isArray(option.options)) return [];
	return option.options.flatMap((entry) => 'options' in entry ? entry.options : [entry]);
}

export function isModelOption(option: AcpConfigOption): boolean {
	return option.id === 'model' || option.category === 'model';
}

export function isReasoningOption(option: AcpConfigOption): boolean {
	return option.category === 'thought_level'
		|| ['reasoning_effort', 'thought_level', 'thinking_level'].includes(option.id);
}

export function isAdditionalModelOption(option: AcpConfigOption): boolean {
	return option.category === 'model_config' && !isModelOption(option) && !isReasoningOption(option);
}

/** Drop removed controls and invalid choices; the adapter owns clamping/defaults. */
export function reconcileConfig(
	options: AcpConfigOption[],
	pending: Record<string, string | boolean> = {},
): Record<string, string | boolean> {
	return Object.fromEntries(options.map((option) => {
		const value = pending[option.id];
		const valid = option.type === 'boolean'
			? typeof value === 'boolean'
			: typeof value === 'string' && selectChoices(option).some((choice) => choice.value === value);
		return [option.id, valid ? value : option.currentValue];
	}));
}

/** Enqueueing is not acknowledgement: retain intent until advertised as current. */
export function unconfirmedConfig(
	options: AcpConfigOption[],
	pending: Record<string, string | boolean>,
): Record<string, string | boolean> {
	return Object.fromEntries(Object.entries(pending).filter(([id, value]) =>
		options.find((option) => option.id === id)?.currentValue !== value
	));
}

/** Some adapters expose reasoning through both modern controls and legacy modes. */
export function duplicatesReasoningMode(mode: AcpConfigOption, options: AcpConfigOption[]): boolean {
	const values = selectChoices(mode).map((choice) => choice.value);
	return options.some((option) => isReasoningOption(option)
		&& values.length === selectChoices(option).length
		&& selectChoices(option).every((choice) => values.includes(choice.value)));
}
