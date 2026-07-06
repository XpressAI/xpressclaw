<script lang="ts">
	import { onMount } from 'svelte';

	// Collaborative live view of the device, shared with the agent through the same
	// /v1/android/* endpoints. Rendered both in the right-rail panel (see +layout)
	// and standalone in a detached window (/android) — the same self-contained
	// component either way. The human can step in any time — you and the agent
	// drive the same screen.

	// `detachable` shows the "pop out" control (rail only); the detached window
	// renders without it. `ondetach` is invoked when that control is clicked.
	// `onreattach` (detached window only) shows the mirror control that returns
	// the view to the app.
	let {
		detachable = false,
		ondetach,
		onreattach
	}: { detachable?: boolean; ondetach?: () => void; onreattach?: () => void } = $props();

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

	// Each device frame takes ~1s to produce. Gate on load — fetch the next frame
	// only after the current finishes (img onload/onerror) plus a short pause — so
	// requests never overlap. Idles while the window is hidden.
	const FRAME_GAP_MS = 400;
	let frameTimer: ReturnType<typeof setTimeout> | undefined;

	function refresh() {
		frameUrl = '/v1/android/screenshot?t=' + Date.now();
	}

	function scheduleNextFrame() {
		clearTimeout(frameTimer);
		frameTimer = setTimeout(() => {
			if (!reachable) return;
			if (typeof document !== 'undefined' && document.hidden) {
				scheduleNextFrame();
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
			const r = await fetch('/api/setup/start-android', {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ avd: avds[0] ?? null })
			});
			if (!r.ok) {
				const detail = await r.json().catch(() => null);
				status = `start failed: ${detail?.error ?? r.status}`;
				starting = false;
				return;
			}
			// Cold boot can take ~60s; poll until it's reachable.
			for (let i = 0; i < 60 && !reachable; i++) {
				await new Promise((r) => setTimeout(r, 2000));
				await checkStatus();
			}
			// Fell through the poll without connecting — the emulator likely failed
			// to boot (bad AVD, no acceleration); the reason is in the server's
			// emulator log. Don't leave the panel silently back on "no device".
			if (!reachable) status = 'emulator did not become reachable — check the server log';
		} catch (e) {
			console.error('start emulator failed', e);
			status = 'start failed: server unreachable';
		}
		starting = false;
	}

	async function post(path: string, body: unknown) {
		try {
			const r = await fetch(path, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify(body)
			});
			// fetch only rejects on network errors, so a 4xx/5xx (device dropped,
			// bad coordinates) would otherwise pass silently while lastAction still
			// claims the action succeeded. Surface the server's error instead.
			if (!r.ok) {
				const detail = await r.json().catch(() => null);
				lastAction = `error: ${detail?.error ?? r.status}`;
			}
		} catch (e) {
			console.error('android action failed', e);
			lastAction = 'error: device unreachable';
		}
		// Pull a fresh frame shortly after acting so the change shows up. Route it
		// through frameTimer (clearing any pending tick first) so this one-off
		// refresh replaces — rather than races — the self-driving frame loop.
		clearTimeout(frameTimer);
		frameTimer = setTimeout(refresh, 350);
	}

	// Press-drag-release: a short press is a tap, a drag is a swipe. Coordinates
	// map to real DEVICE pixels (the frame is downscaled, so imgEl.naturalWidth
	// would be the JPEG size — wrong; /tap and /swipe want device px).
	const SWIPE_THRESHOLD = 12; // device px of travel before it counts as a swipe
	let pressStart: { x: number; y: number; t: number } | null = null;

	function toDevice(e: PointerEvent): { x: number; y: number } | null {
		if (!imgEl || !deviceW || !deviceH) return null;
		const rect = imgEl.getBoundingClientRect();
		// Clamp to the screen: with setPointerCapture a drag can end past the
		// letterboxed frame edge, which would otherwise post negative or
		// beyond-screen coordinates that `input tap`/`swipe` reject or misplace.
		const clamp = (v: number, max: number) => Math.max(0, Math.min(max, Math.round(v)));
		return {
			x: clamp(((e.clientX - rect.left) / rect.width) * deviceW, deviceW - 1),
			y: clamp(((e.clientY - rect.top) / rect.height) * deviceH, deviceH - 1)
		};
	}

	function handlePointerDown(e: PointerEvent) {
		const p = toDevice(e);
		if (!p) return;
		e.preventDefault(); // stop the browser's native image drag (the "drags the image" bug)
		pressStart = { ...p, t: Date.now() };
		// Capture so we still get pointerup if the drag ends outside the image.
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
	}

	function handlePointerUp(e: PointerEvent) {
		const start = pressStart;
		pressStart = null;
		const end = toDevice(e);
		if (!start || !end) return;
		const dist = Math.hypot(end.x - start.x, end.y - start.y);
		if (dist < SWIPE_THRESHOLD) {
			lastAction = `tap (${end.x}, ${end.y})`;
			post('/v1/android/tap', { x: end.x, y: end.y });
		} else {
			// Swipe duration follows the actual drag time — a flick is fast, a
			// deliberate drag is slow — clamped to a sane range.
			const ms = Math.min(800, Math.max(50, Date.now() - start.t));
			lastAction = `swipe (${start.x},${start.y})→(${end.x},${end.y})`;
			post('/v1/android/swipe', { x1: start.x, y1: start.y, x2: end.x, y2: end.y, ms });
		}
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
		// The frame loop is self-driving: the <img> mounts once reachable and each
		// onload/onerror schedules the next frame. We only poll status here.
		const statusTimer = setInterval(checkStatus, 5000);
		return () => {
			clearInterval(statusTimer);
			clearTimeout(frameTimer);
		};
	});
</script>

<div class="flex h-full flex-col">
	<div class="flex h-11 flex-shrink-0 items-center gap-2 border-b border-border px-3">
		<span class="text-sm font-semibold">Android</span>
		<span
			class="rounded-full px-2 py-0.5 text-[10px] font-medium {reachable
				? 'bg-emerald-500/15 text-emerald-400'
				: 'bg-muted text-muted-foreground'}">{status}</span
		>
		{#if detachable}
			<button
				onclick={ondetach}
				title="Open in a separate window"
				aria-label="Open in a separate window"
				class="ml-auto flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
			>
				<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"
					><path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M13.5 6H5.25A2.25 2.25 0 003 8.25v10.5A2.25 2.25 0 005.25 21h10.5A2.25 2.25 0 0018 18.75V10.5m-10.5 6L21 3m0 0h-5.25M21 3v5.25"
					/></svg
				>
			</button>
		{/if}
		{#if onreattach}
			<button
				onclick={onreattach}
				title="Return to the app"
				aria-label="Return to the app"
				class="ml-auto flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
			>
				<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"
					><path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 9V4.5M9 9H4.5M9 9L3.75 3.75M9 15v4.5M9 15H4.5M9 15l-5.25 5.25M15 9h4.5M15 9V4.5M15 9l5.25-5.25M15 15h4.5M15 15v4.5m0-4.5l5.25 5.25"
					/></svg
				>
			</button>
		{/if}
	</div>

	{#if !reachable}
		<div class="flex flex-1 items-center justify-center p-4">
			<div class="rounded-xl border border-border bg-muted/30 p-5 text-center">
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
						class="mt-3 rounded-md bg-primary px-4 py-2 text-sm font-semibold text-primary-foreground hover:bg-primary/90"
						>Start emulator</button
					>
				{:else if !installed}
					<p class="text-sm font-medium">Android SDK not ready</p>
					<p class="mt-1 text-xs text-muted-foreground">
						Install the SDK + a system image and create an AVD, then reconnect. Run
						<code>xpressclaw android doctor</code> to see what's missing.
					</p>
				{:else}
					<p class="text-sm font-medium">Connecting to a device…</p>
					<p class="mt-1 text-xs text-muted-foreground">Or plug in a device with USB debugging.</p>
				{/if}
			</div>
		</div>
	{:else}
		<div class="flex min-h-0 flex-1 flex-col gap-3 p-3">
			<!-- Phone: the OUTLINE is sized from the real device dimensions
			     (deviceW×deviceH from /status), not from whatever frame is loaded —
			     so it stays a correct device shape even when the screen is black,
			     asleep, or still loading. The wrapper is a size container and the
			     bezel width is min(fit-to-width, fit-to-height × ratio) in cq units:
			     a true contain-fit that tracks BOTH axes on resize. (A plain
			     `height: 100%` breaks here — percentage heights don't reliably
			     resolve through flex items, so vertical resizes were ignored.) -->
			<div class="flex min-h-0 flex-1 items-center justify-center p-2 [container-type:size]">
				<div
					class="rounded-[1.75rem] bg-neutral-900 p-1.5 shadow-lg ring-1 ring-white/10"
					style="aspect-ratio: {deviceW || 1080} / {deviceH || 2400}; width: min(100cqw, calc(100cqh * {(deviceW ||
						1080) / (deviceH || 2400)}));"
				>
					<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
					<img
						bind:this={imgEl}
						src={frameUrl}
						alt="Android device screen"
						draggable="false"
						onpointerdown={handlePointerDown}
						onpointerup={handlePointerUp}
						onload={scheduleNextFrame}
						onerror={scheduleNextFrame}
						class="block h-full w-full rounded-[1.25rem] object-contain cursor-crosshair select-none touch-none"
					/>
				</div>
			</div>

			<div class="flex flex-shrink-0 items-center justify-center gap-2">
				<button
					onclick={() => sendKey('KEYCODE_BACK', 'back')}
					title="Back"
					aria-label="Back"
					class="flex h-8 w-8 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-accent hover:text-foreground"
				>
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"
						><path stroke-linecap="round" stroke-linejoin="round" d="M10.5 19.5 3 12m0 0 7.5-7.5M3 12h18" /></svg
					>
				</button>
				<button
					onclick={() => sendKey('KEYCODE_HOME', 'home')}
					title="Home"
					aria-label="Home"
					class="flex h-8 w-8 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-accent hover:text-foreground"
				>
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"
						><path
							stroke-linecap="round"
							stroke-linejoin="round"
							d="M2.25 12l8.954-8.955c.44-.439 1.152-.439 1.591 0L21.75 12M4.5 9.75v10.125c0 .621.504 1.125 1.125 1.125H9.75v-4.875c0-.621.504-1.125 1.125-1.125h2.25c.621 0 1.125.504 1.125 1.125V21h4.125c.621 0 1.125-.504 1.125-1.125V9.75"
						/></svg
					>
				</button>
				<button
					onclick={() => sendKey('KEYCODE_APP_SWITCH', 'recents')}
					title="Recent apps"
					aria-label="Recent apps"
					class="flex h-8 w-8 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-accent hover:text-foreground"
				>
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"
						><rect x="4.5" y="4.5" width="15" height="15" rx="2" /></svg
					>
				</button>
			</div>
			<div class="flex flex-shrink-0 items-center gap-1.5">
				<input
					bind:value={textInput}
					placeholder="type text, Enter to send…"
					onkeydown={(e) => e.key === 'Enter' && sendText()}
					class="min-w-0 flex-1 rounded-md border border-input bg-background px-2.5 py-1 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
				/>
				<button
					onclick={sendText}
					class="rounded-md bg-primary px-2.5 py-1 text-xs text-primary-foreground hover:bg-primary/90"
					>Send</button
				>
			</div>

			{#if lastAction}
				<p class="flex-shrink-0 text-center text-[10px] text-muted-foreground">last: {lastAction}</p>
			{/if}
		</div>
	{/if}
</div>
