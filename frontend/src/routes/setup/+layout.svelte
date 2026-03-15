<script lang="ts">
	import '../../app.css';

	let { children } = $props();
</script>

<div class="flex min-h-screen items-center justify-center bg-background p-4">
	<div class="w-full max-w-2xl">
		<!-- Header -->
		<div class="mb-8 text-center">
			<img src="/icon.png" alt="xpressclaw" class="mx-auto mb-4 h-12 w-12 rounded-xl" />
			<h1 class="text-2xl font-bold text-foreground">Welcome to xpressclaw</h1>
			<p class="mt-1 text-sm text-muted-foreground">Let's get your agent runtime set up</p>
		</div>

		{@render children()}
	</div>
</div>
