<script lang="ts">
	interface PreviewAttachment {
		id?: string;
		name: string;
		src: string;
		mimeType?: string;
		size?: number;
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

	function formatBytes(bytes: number | undefined): string {
		if (bytes === undefined) return '';
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	}

	function artifactKind(mimeType: string | undefined): { mark: string; label: string } {
		switch (mimeType) {
			case 'application/vnd.openxmlformats-officedocument.presentationml.presentation':
				return { mark: 'P', label: 'PowerPoint presentation' };
			case 'application/vnd.openxmlformats-officedocument.wordprocessingml.document':
				return { mark: 'W', label: 'Word document' };
			case 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet':
				return { mark: 'X', label: 'Excel workbook' };
			case 'application/pdf':
				return { mark: 'PDF', label: 'PDF document' };
			default:
				return { mark: '↓', label: 'File attachment' };
		}
	}
</script>

{#if attachments.length > 0}
	<div class="flex flex-wrap gap-2 {message ? 'mt-2' : 'px-4 pb-2'}" data-image-attachments data-message-attachments={message || undefined}>
		{#each attachments as attachment, index (attachment.id ?? `${attachment.name}-${index}`)}
			{@const kind = artifactKind(attachment.mimeType)}
			<div class="group relative overflow-hidden rounded-lg border border-border/70 bg-background/50">
				{#if !attachment.mimeType || attachment.mimeType.startsWith('image/')}
					<a href={attachment.src} target="_blank" rel="noopener noreferrer" aria-label="Open {attachment.name || `image ${index + 1}`}" class="block">
						<img
							src={attachment.src}
							alt={attachment.name || `Attached image ${index + 1}`}
							class="object-cover {message ? 'h-28 max-w-48' : 'h-16 w-20'}"
						/>
					</a>
				{:else}
					<a
						href={attachment.src}
						target="_blank"
						rel="noopener noreferrer"
						aria-label="Download {attachment.name}"
						class="flex min-w-52 items-center gap-3 px-3 py-2.5 text-left hover:bg-accent/40"
					>
						<span aria-hidden="true" class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-[10px] font-semibold text-primary">{kind.mark}</span>
						<span class="min-w-0">
							<span class="block max-w-64 truncate text-xs font-medium text-foreground">{attachment.name}</span>
							<span class="block text-[10px] text-muted-foreground">{kind.label}{attachment.size === undefined ? '' : ` · ${formatBytes(attachment.size)}`}</span>
						</span>
					</a>
				{/if}
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
