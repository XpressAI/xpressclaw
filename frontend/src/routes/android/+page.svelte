<script lang="ts">
	// Detached Android live view — the same component as the right-rail panel,
	// rendered full-window. Opened from the rail's "pop out" control (see
	// +layout.svelte detachAndroid), as a native window in the desktop app or a
	// browser popup on the web. Renders chromeless (no sidebar/rail); the layout
	// treats /android as a bare route.
	import AndroidLiveView from '$lib/AndroidLiveView.svelte';
</script>

<svelte:head><title>Android — xpressclaw</title></svelte:head>

<div class="h-screen w-screen bg-background">
	<AndroidLiveView />
</div>
