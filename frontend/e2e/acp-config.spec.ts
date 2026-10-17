import { expect, test } from '@playwright/test';
import { duplicatesReasoningMode, reconcileConfig, unconfirmedConfig } from '../src/lib/acpConfig';
import type { AcpConfigOption } from '../src/lib/api';

const reasoning: AcpConfigOption = {
	id: 'thought_level', name: 'Thinking', category: 'thought_level', type: 'select', currentValue: 'high',
	options: [{ group: 'levels', name: 'Supported', options: [{ value: 'high', name: 'High' }, { value: 'max', name: 'Max' }] }],
};

test('refreshed controls drop invalid and removed settings, retaining valid pending choices', () => {
	const options: AcpConfigOption[] = [reasoning, {
		id: 'fast', name: 'Fast', type: 'boolean', currentValue: false,
	}];
	expect(reconcileConfig(options, { thought_level: 'off', removed: true, fast: 'true' })).toEqual({
		thought_level: 'high', fast: false,
	});
	expect(reconcileConfig(options, { thought_level: 'max', fast: true })).toEqual({
		thought_level: 'max', fast: true,
	});
	expect(reconcileConfig(options)).toEqual({ thought_level: 'high', fast: false });
});

test('only advertised current values acknowledge staged controls', () => {
	const pending = { thought_level: 'max', fast: false, not_advertised_yet: 'waiting' };
	const options: AcpConfigOption[] = [reasoning, { id: 'fast', name: 'Fast', type: 'boolean', currentValue: false }];
	const retained = unconfirmedConfig(options, pending);
	expect(retained).toEqual({ thought_level: 'max', not_advertised_yet: 'waiting' });
	expect(unconfirmedConfig([{ ...reasoning, currentValue: 'max' }], retained)).toEqual({ not_advertised_yet: 'waiting' });
	expect(pending).toEqual({ thought_level: 'max', fast: false, not_advertised_yet: 'waiting' });
});

test('legacy reasoning aliases are hidden without hiding an independent mode selector', () => {
	const mode: AcpConfigOption = {
		id: 'mode', name: 'Mode', type: 'select', currentValue: 'high',
		options: [{ value: 'max', name: 'Thinking: max' }, { value: 'high', name: 'Thinking: high' }],
	};
	expect(duplicatesReasoningMode(mode, [reasoning])).toBe(true);
	expect(duplicatesReasoningMode({ ...mode, options: [{ value: 'plan', name: 'Plan' }] }, [reasoning])).toBe(false);
});
