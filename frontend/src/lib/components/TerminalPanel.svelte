<script lang="ts">
	import { onMount } from 'svelte';
	import '@xterm/xterm/css/xterm.css';
	import { request } from '$lib/api';

	let { agentId }: { agentId: string } = $props();
	let host = $state<HTMLDivElement>();
	let connected = $state(false);
	let connecting = $state(true);
	let error = $state('');
	let reconnect = $state<() => void>(() => {});

	onMount(() => {
		let disposed = false;
		let cleanup = () => {};
		void (async () => {
			const [{ Terminal }, { FitAddon }, { WebLinksAddon }] = await Promise.all([
				import('@xterm/xterm'),
				import('@xterm/addon-fit'),
				import('@xterm/addon-web-links'),
			]);
			if (disposed || !host) return;
			const terminal = new Terminal({
				cursorBlink: true,
				convertEol: true,
				fontSize: 13,
				fontFamily: "'JetBrains Mono', 'Cascadia Code', 'SFMono-Regular', Consolas, monospace",
				scrollback: 10_000,
				theme: terminalTheme(),
			});
			const fitAddon = new FitAddon();
			terminal.loadAddon(fitAddon);
			terminal.loadAddon(new WebLinksAddon((_event, uri) => {
				window.open(uri, '_blank', 'noopener,noreferrer');
			}));
			terminal.open(host);
			requestAnimationFrame(() => fitAddon.fit());

			let websocket: WebSocket | null = null;
			const encoder = new TextEncoder();
			const decoder = new TextDecoder();

			function connect() {
				websocket?.close();
				connected = false;
				connecting = true;
				error = '';
				terminal.writeln('\x1b[90mConnecting to the retained project environment…\x1b[0m');
				const url = new URL(`/api/workspaces/${encodeURIComponent(agentId)}/terminal`, window.location.href);
				url.protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
				url.searchParams.set('columns', String(terminal.cols));
				url.searchParams.set('rows', String(terminal.rows));
				websocket = new WebSocket(url);
				websocket.binaryType = 'arraybuffer';
				websocket.onmessage = (event) => {
					if (typeof event.data === 'string') {
						try {
							const control = JSON.parse(event.data) as { type?: string; message?: string };
							if (control.type === 'ready') {
								connected = true;
								connecting = false;
								terminal.focus();
							} else if (control.type === 'error') {
								error = control.message || 'The terminal failed to start.';
								connecting = false;
								terminal.writeln(`\r\n\x1b[31m${error}\x1b[0m`);
							} else if (control.type === 'exit') {
								connected = false;
								connecting = false;
								terminal.writeln('\r\n\x1b[90mTerminal exited.\x1b[0m');
							}
						} catch {
							terminal.write(event.data);
						}
						return;
					}
					if (event.data instanceof ArrayBuffer) {
						terminal.write(decoder.decode(new Uint8Array(event.data), { stream: true }));
					} else if (event.data instanceof Blob) {
						void event.data.arrayBuffer().then((buffer) => {
							terminal.write(decoder.decode(new Uint8Array(buffer), { stream: true }));
						});
					}
				};
			websocket.onerror = () => {
				connected = false;
				connecting = false;
				error = 'Could not connect to the retained project environment.';
				// WebSocket does not expose an HTTP 401 handshake to JavaScript.
				// Probe a protected endpoint so expired sessions recover through
				// the normal login redirect instead of looking like a terminal fault.
				void request(`/api/agents/${encodeURIComponent(agentId)}`).catch(() => undefined);
			};
				websocket.onclose = () => {
					connected = false;
					connecting = false;
				};
			}

			const inputSubscription = terminal.onData((data) => {
				if (websocket?.readyState === WebSocket.OPEN) websocket.send(encoder.encode(data));
			});
			const resizeSubscription = terminal.onResize(({ cols, rows }) => {
				if (websocket?.readyState === WebSocket.OPEN) {
					websocket.send(JSON.stringify({ type: 'resize', columns: cols, rows }));
				}
			});
			const resizeObserver = new ResizeObserver(() => {
				try { fitAddon.fit(); } catch {}
			});
			resizeObserver.observe(host);
			const themeObserver = new MutationObserver(() => {
				terminal.options.theme = terminalTheme();
			});
			themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
			reconnect = connect;
			connect();

			cleanup = () => {
				websocket?.close();
				inputSubscription.dispose();
				resizeSubscription.dispose();
				resizeObserver.disconnect();
				themeObserver.disconnect();
				terminal.dispose();
			};
		})();
		return () => {
			disposed = true;
			cleanup();
		};
	});

	function terminalTheme() {
		const dark = document.documentElement.classList.contains('dark');
		return dark
			? {
				background: '#0d0f18', foreground: '#e4e7ef', cursor: '#7c93ff',
				selectionBackground: '#33467a', black: '#141722', red: '#f87171', green: '#34d399',
				yellow: '#fbbf24', blue: '#60a5fa', magenta: '#c084fc', cyan: '#22d3ee', white: '#e5e7eb',
			}
			: {
				background: '#ffffff', foreground: '#1f2937', cursor: '#365bd6',
				selectionBackground: '#c7d2fe', black: '#111827', red: '#dc2626', green: '#059669',
				yellow: '#d97706', blue: '#2563eb', magenta: '#9333ea', cyan: '#0891b2', white: '#f3f4f6',
			};
	}
</script>

<div class="flex h-full min-h-[12rem] flex-col bg-background" data-project-terminal>
	<div class="flex h-8 shrink-0 items-center justify-between border-b border-border px-3 text-[11px] text-muted-foreground">
		<div class="flex items-center gap-2">
			<span class="h-1.5 w-1.5 rounded-full {connected ? 'bg-emerald-500' : connecting ? 'animate-pulse bg-amber-500' : 'bg-muted-foreground'}"></span>
			<span>{connected ? 'Container terminal' : connecting ? 'Connecting…' : 'Terminal disconnected'}</span>
		</div>
		{#if !connected && !connecting}
			<button type="button" onclick={() => reconnect()} class="rounded border border-border px-2 py-0.5 hover:bg-accent">Reconnect</button>
		{/if}
	</div>
	<div bind:this={host} class="min-h-0 flex-1 p-2"></div>
	{#if error}
		<div class="shrink-0 border-t border-destructive/30 bg-destructive/5 px-3 py-1.5 text-[11px] text-destructive">{error}</div>
	{/if}
</div>
