<script lang="ts">
	import { onMount } from 'svelte';
	import type { Monaco } from '$lib/monaco';

	interface Props {
		value: string;
		path: string;
		readOnly?: boolean;
		language?: string;
		onChange?: (value: string) => void;
		onSave?: () => void;
	}

	let { value, path, readOnly = false, language, onChange, onSave }: Props = $props();
	let host = $state<HTMLDivElement>();
	let ready = $state(false);
	let editor: import('monaco-editor').editor.IStandaloneCodeEditor | null = null;
	let monaco: Monaco | null = null;
	let applyingExternalValue = false;

	$effect(() => {
		if (!editor || editor.getValue() === value) return;
		applyingExternalValue = true;
		editor.setValue(value);
		applyingExternalValue = false;
	});

	$effect(() => {
		editor?.updateOptions({ readOnly });
	});

	onMount(() => {
		let disposed = false;
		let disposeEditor = () => {};
		void (async () => {
			const module = await import('$lib/monaco');
			if (disposed || !host) return;
			monaco = module.loadMonaco();
			const model = monaco.editor.createModel(
				value,
				language || languageForPath(path),
				monaco.Uri.parse(`inmemory://xpressclaw/${encodeURIComponent(path || 'untitled')}`),
			);
			editor = monaco.editor.create(host, {
				model,
				theme: document.documentElement.classList.contains('dark') ? 'vs-dark' : 'vs',
				readOnly,
				automaticLayout: true,
				fontSize: 13,
				fontFamily: "'JetBrains Mono', 'Cascadia Code', 'SFMono-Regular', Consolas, monospace",
				fontLigatures: true,
				minimap: { enabled: true, maxColumn: 100 },
				scrollBeyondLastLine: false,
				renderWhitespace: 'selection',
				wordWrap: 'off',
				bracketPairColorization: { enabled: true },
				padding: { top: 10, bottom: 10 },
			});
			const change = editor.onDidChangeModelContent(() => {
				if (!applyingExternalValue) onChange?.(editor?.getValue() ?? '');
			});
			if (onSave) {
				editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => onSave());
			}
			const themeObserver = new MutationObserver(() => {
				monaco?.editor.setTheme(document.documentElement.classList.contains('dark') ? 'vs-dark' : 'vs');
			});
			themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
			ready = true;
			disposeEditor = () => {
				change.dispose();
				themeObserver.disconnect();
				editor?.dispose();
				model.dispose();
				editor = null;
			};
		})();
		return () => {
			disposed = true;
			disposeEditor();
		};
	});

	function languageForPath(filePath: string): string {
		const name = filePath.split('/').pop()?.toLowerCase() ?? '';
		const extension = name.includes('.') ? name.split('.').pop() ?? '' : '';
		if (name === 'dockerfile') return 'dockerfile';
		if (name === 'makefile') return 'makefile';
		if (name === 'cargo.toml' || extension === 'toml') return 'ini';
		return ({
			rs: 'rust', ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
			json: 'json', jsonc: 'json', css: 'css', scss: 'scss', less: 'less', html: 'html',
			svelte: 'html', vue: 'html', py: 'python', rb: 'ruby', go: 'go', java: 'java',
			kt: 'kotlin', kts: 'kotlin', c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', hpp: 'cpp',
			cs: 'csharp', sh: 'shell', bash: 'shell', zsh: 'shell', ps1: 'powershell',
			yml: 'yaml', yaml: 'yaml', md: 'markdown', sql: 'sql', xml: 'xml',
			graphql: 'graphql', gql: 'graphql', php: 'php', swift: 'swift', dart: 'dart',
			diff: 'diff',
		} as Record<string, string>)[extension] ?? 'plaintext';
	}
</script>

<div class="relative h-full min-h-[18rem] w-full overflow-hidden bg-background" data-monaco-editor>
	<div bind:this={host} class="absolute inset-0"></div>
	{#if !ready}
		<div class="absolute inset-0 flex items-center justify-center text-xs text-muted-foreground">Loading editor…</div>
	{/if}
</div>
