<script lang="ts">
	import '../../app.css';

	let { children } = $props();
</script>

<div class="flex min-h-screen items-center justify-center bg-background p-4">
	<div class="w-full max-w-2xl">
		<!-- Header -->
		<div class="mb-8 text-center">
			<img src="/icon.png" alt="xpressclaw" class="mx-auto mb-4 h-12 w-12 rounded-xl" />
			<h1 class="text-2xl font-bold text-foreground">Set up xpressclaw</h1>
			<p class="mt-1 text-sm text-muted-foreground">Give a native CLI a project and a durable place to work</p>
		</div>

		{@render children()}
	</div>
</div>
