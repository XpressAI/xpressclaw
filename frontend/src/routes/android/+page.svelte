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
	// Real device resolution (from /v1/android/status). The frame is downscaled,
	// so we map clicks to these, not to the image's own pixel size.
	let deviceW = $state(0);
	let deviceH = $state(0);

	// Emulator lifecycle (mirrors the Docker "installed? / running? / start it" flow)
	let installed = $state(false);
	let canStart = $state(false);
	let avds = $state<string[]>([]);
	let starting = $state(false);

	// Each device frame takes ~1s to produce. We must NOT poll on a fixed interval
	// faster than that: pointing <img src> at a new frame before the previous one
	// finishes cancels the in-flight load, so the image never updates (it looked
	// "frozen" until a re-navigate). Instead, gate on load — fetch the next frame
	// only after the current finishes (img onload/onerror), following a short pause.
	// This adapts to latency, never overlaps requests, and idles when hidden.
	const FRAME_GAP_MS = 400; // pause after each frame loads before fetching the next
	let frameTimer: ReturnType<typeof setTimeout> | undefined;

	function refresh() {
		frameUrl = '/v1/android/screenshot?t=' + Date.now();
	}

	function scheduleNextFrame() {
		clearTimeout(frameTimer);
		frameTimer = setTimeout(() => {
			if (!reachable) return; // device gone; the <img> remounts & re-kicks later
			if (typeof document !== 'undefined' && document.hidden) {
				scheduleNextFrame(); // window hidden — idle without hammering the device
				return;
			}
			refresh();
		}, FRAME_GAP_MS);
	}

	async function checkStatus() {
		try {
			const r = await fetch('/v1/android/status');
			if (!r.ok) throw new Error();
			const j = await r.json();
			reachable = !!j.reachable;
			if (j.width && j.height) {
				deviceW = j.width;
				deviceH = j.height;
			}
			status = reachable ? 'connected' : 'no device reachable';
		} catch {
			reachable = false;
			status = 'android control unavailable';
		}
		if (!reachable) await checkLifecycle();
	}

	async function checkLifecycle() {
		try {
			const j = await (await fetch('/api/setup/check-android-sdk')).json();
			installed = !!j.installed;
			canStart = !!j.can_start;
			avds = j.sdk?.avds ?? [];
		} catch {
			/* leave defaults */
		}
	}

	async function startEmulator() {
		starting = true;
		status = 'starting emulator…';
		try {
			await fetch('/api/setup/start-android', {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ avd: avds[0] ?? null })
			});
			// Cold boot can take ~60s; poll until it's reachable.
			for (let i = 0; i < 60 && !reachable; i++) {
				await new Promise((r) => setTimeout(r, 2000));
				await checkStatus();
			}
		} catch (e) {
			console.error('start emulator failed', e);
		}
		starting = false;
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
		if (!imgEl || !deviceW || !deviceH) return;
		const rect = imgEl.getBoundingClientRect();
		// Map the click fraction to real DEVICE pixels. The fraction is
		// resolution-independent; the frame is downscaled, so imgEl.naturalWidth
		// would be the JPEG size (wrong) — /tap wants device pixels.
		const x = Math.round(((e.clientX - rect.left) / rect.width) * deviceW);
		const y = Math.round(((e.clientY - rect.top) / rect.height) * deviceH);
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
		// The frame loop is self-driving: the <img> mounts once reachable, and each
		// onload/onerror schedules the next frame. We only poll status here.
		const statusTimer = setInterval(checkStatus, 5000);
		return () => {
			clearInterval(statusTimer);
			clearTimeout(frameTimer);
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

	{#if !reachable}
		<!-- Lifecycle gate — no device up. Mirrors the Docker "start it" prompt. -->
		<div class="rounded-xl border border-border bg-muted/30 p-8 text-center">
			{#if starting}
				<p class="text-sm font-medium">Starting emulator…</p>
				<p class="mt-1 text-xs text-muted-foreground">Cold boot can take up to a minute.</p>
			{:else if installed && canStart}
				<p class="text-sm font-medium">No emulator running</p>
				<p class="mt-1 text-xs text-muted-foreground">
					{avds.length ? `Will boot: ${avds[0]}` : 'A managed emulator is available.'}
				</p>
				<button
					onclick={startEmulator}
					class="mt-4 rounded-md bg-primary px-4 py-2 text-sm font-semibold text-primary-foreground hover:bg-primary/90"
					>Start emulator</button
				>
			{:else if !installed}
				<p class="text-sm font-medium">Android SDK not ready</p>
				<p class="mt-1 text-xs text-muted-foreground">
					Install the Android SDK + a system image and create an AVD, then refresh.
					Run <code>xpressclaw android doctor</code> to see what's missing.
				</p>
			{:else}
				<p class="text-sm font-medium">Connecting to a device…</p>
				<p class="mt-1 text-xs text-muted-foreground">Or plug in a device with USB debugging.</p>
			{/if}
		</div>
	{:else}
	<div class="flex flex-col items-center gap-4">
		<div class="overflow-hidden rounded-xl border border-border bg-black">
			<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
			<img
				bind:this={imgEl}
				src={frameUrl}
				alt="Android device screen"
				onclick={handleClick}
				onload={scheduleNextFrame}
				onerror={scheduleNextFrame}
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
	{/if}
</div>
