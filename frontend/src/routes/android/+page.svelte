<script lang="ts">
	import { onMount } from 'svelte';

	// Collaborative live view: poll the device screen and forward clicks/keys to
	// the same /v1/android/* endpoints the agent uses. Human and agent share the
	// device — the human can step in any time. See ADR-024.

	let frameUrl = $state('/v1/android/screenshot?t=0');
	let reachable = $state(false);
	let imgEl: HTMLImageElement | undefined = $state();
	let textInput = $state('');
	let status = $state('connecting…');
	let lastAction = $state<string | null>(null);

	function refresh() {
		frameUrl = '/v1/android/screenshot?t=' + Date.now();
	}

	async function checkStatus() {
		try {
			const r = await fetch('/v1/android/status');
			if (!r.ok) throw new Error();
			const j = await r.json();
			reachable = !!j.reachable;
			status = reachable ? 'connected' : 'no device reachable';
		} catch {
			reachable = false;
			status = 'android control unavailable';
		}
	}

	async function post(path: string, body: unknown) {
		try {
			await fetch(path, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify(body)
			});
		} catch (e) {
			console.error('android action failed', e);
		}
		// Pull a fresh frame shortly after acting so the change shows up.
		setTimeout(refresh, 350);
	}

	function handleClick(e: MouseEvent) {
		if (!imgEl || !imgEl.naturalWidth) return;
		const rect = imgEl.getBoundingClientRect();
		const x = Math.round(((e.clientX - rect.left) / rect.width) * imgEl.naturalWidth);
		const y = Math.round(((e.clientY - rect.top) / rect.height) * imgEl.naturalHeight);
		lastAction = `tap (${x}, ${y})`;
		post('/v1/android/tap', { x, y });
	}

	function sendKey(key: string, label: string) {
		lastAction = label;
		post('/v1/android/key', { key });
	}

	function sendText() {
		if (!textInput) return;
		lastAction = `typed "${textInput}"`;
		post('/v1/android/input-text', { text: textInput });
		textInput = '';
	}

	onMount(() => {
		checkStatus();
		refresh();
		const frameTimer = setInterval(refresh, 700);
		const statusTimer = setInterval(checkStatus, 5000);
		return () => {
			clearInterval(frameTimer);
			clearInterval(statusTimer);
		};
	});
</script>

<div class="mx-auto max-w-3xl p-6">
	<div class="mb-1 flex items-center gap-3">
		<h1 class="text-2xl font-bold">Android</h1>
		<span
			class="rounded-full px-2 py-0.5 text-xs font-medium {reachable
				? 'bg-emerald-500/15 text-emerald-400'
				: 'bg-muted text-muted-foreground'}"
		>
			{status}
		</span>
	</div>
	<p class="mb-4 text-sm text-muted-foreground">
		Watch the device live and click to take over — you and the agent share the same screen.
	</p>

	<div class="flex flex-col items-center gap-4">
		<div class="overflow-hidden rounded-xl border border-border bg-black">
			<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
			<img
				bind:this={imgEl}
				src={frameUrl}
				alt="Android device screen"
				onclick={handleClick}
				class="block max-h-[78vh] w-auto cursor-crosshair"
			/>
		</div>

		<div class="flex flex-wrap items-center justify-center gap-2">
			<button
				onclick={() => sendKey('KEYCODE_BACK', 'back')}
				class="rounded-md border border-border px-3 py-1.5 text-sm hover:bg-accent">◀ Back</button
			>
			<button
				onclick={() => sendKey('KEYCODE_HOME', 'home')}
				class="rounded-md border border-border px-3 py-1.5 text-sm hover:bg-accent">⬤ Home</button
			>
			<button
				onclick={() => sendKey('KEYCODE_APP_SWITCH', 'recents')}
				class="rounded-md border border-border px-3 py-1.5 text-sm hover:bg-accent">▢ Recents</button
			>
			<input
				bind:value={textInput}
				placeholder="type text, Enter to send…"
				onkeydown={(e) => e.key === 'Enter' && sendText()}
				class="w-56 rounded-md border border-input bg-background px-3 py-1.5 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
			/>
			<button
				onclick={sendText}
				class="rounded-md bg-primary px-3 py-1.5 text-sm text-primary-foreground hover:bg-primary/90"
				>Send</button
			>
		</div>

		{#if lastAction}
			<p class="text-xs text-muted-foreground">last: {lastAction}</p>
		{/if}
	</div>
</div>
