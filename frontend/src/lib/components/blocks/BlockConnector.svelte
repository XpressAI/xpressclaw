<script lang="ts">
	let { label }: { label?: string } = $props();
</script>

<div class="flex items-center justify-center py-0.5">
	<div class="flex flex-col items-center">
		<div class="w-0.5 h-3 bg-border/60"></div>
		{#if label}
			<span class="text-[9px] text-muted-foreground/60 font-mono px-1">{label}</span>
		{/if}
		<div class="w-0.5 h-3 bg-border/60"></div>
	</div>
</div>
