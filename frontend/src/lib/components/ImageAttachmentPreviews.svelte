<script lang="ts">
	interface PreviewAttachment {
		id?: string;
		name: string;
		src: string;
	}

	let {
		attachments,
		onremove,
		message = false,
	}: {
		attachments: PreviewAttachment[];
		onremove?: (index: number) => void;
		message?: boolean;
	} = $props();
</script>

{#if attachments.length > 0}
	<div class="flex flex-wrap gap-2 {message ? 'mt-2' : 'px-4 pb-2'}" data-image-attachments>
		{#each attachments as attachment, index (attachment.id ?? `${attachment.name}-${index}`)}
			<div class="group relative overflow-hidden rounded-lg border border-border/70 bg-background/50">
				<a href={attachment.src} target="_blank" rel="noreferrer" aria-label="Open {attachment.name || `image ${index + 1}`}" class="block">
					<img
						src={attachment.src}
						alt={attachment.name || `Attached image ${index + 1}`}
						class="object-cover {message ? 'h-28 max-w-48' : 'h-16 w-20'}"
					/>
				</a>
				{#if onremove}
					<button
						type="button"
						onclick={() => onremove?.(index)}
						aria-label="Remove {attachment.name || `image ${index + 1}`}"
						class="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-black/75 text-xs text-white opacity-90 transition-opacity hover:bg-black group-hover:opacity-100"
					>×</button>
				{/if}
			</div>
		{/each}
	</div>
{/if}
