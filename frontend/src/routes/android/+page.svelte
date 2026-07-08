<script lang="ts">
	// Detached Android live view — the same component as the right-rail panel,
	// rendered full-window. Opened from the rail's "pop out" control (see
	// +layout.svelte detachAndroid), as a native window in the desktop app or a
	// browser popup on the web. Renders chromeless (no sidebar/rail); the layout
	// treats /android as a bare route.
	import AndroidLiveView from '$lib/AndroidLiveView.svelte';

	// Reattach = close this window; the main window notices — tauri://destroyed
	// listener on desktop, closed-poll in the browser — and reopens the rail
	// panel (+layout reattachAndroid).
	//
	// On desktop this MUST be Tauri's close() IPC, never window.close(): wry's
	// window.close() handler destroys only the WRY_WEBVIEW container HWND inside
	// the native window, leaving a live white window and a stale 'android-live'
	// label that blocks every future detach. close() needs core:window:allow-close
	// granted to this window (capabilities/remote-webview.json).
	async function reattach() {
		if ('__TAURI_INTERNALS__' in window) {
			try {
				const { getCurrentWindow } = await import('@tauri-apps/api/window');
				await getCurrentWindow().close();
			} catch (e) {
				// Deliberately no window.close() fallback here — that's the
				// white-screen path. Leave the window up and surface the error.
				console.error('reattach close failed', e);
			}
		} else {
			window.close();
		}
	}
</script>

<svelte:head><title>Android — xpressclaw</title></svelte:head>

<div class="h-screen w-screen bg-background">
	<AndroidLiveView onreattach={reattach} />
</div>
