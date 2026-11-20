<script lang="ts">
	import { onMount } from 'svelte';
	import { settings, type SpeechSettings } from '$lib/api';
	import { speechSettings } from '$lib/speech';

	let config = $state<SpeechSettings | null>(null);
	let apiKey = $state('');
	let clearKey = $state(false);
	let saving = $state(false);
	let saved = $state(false);
	let error = $state('');

	async function load() {
		error = '';
		try {
			config = await settings.getSpeech();
			speechSettings.set(config);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not load speech settings.';
		}
	}
	onMount(() => { void load(); });

	async function save(event: SubmitEvent) {
		event.preventDefault();
		if (!config || saving) return;
		saving = true;
		saved = false;
		error = '';
		try {
			const { has_api_key, ...fields } = config;
			config = await settings.putSpeech({
				...fields,
				...(clearKey ? { api_key: '' } : apiKey.trim() ? { api_key: apiKey.trim() } : {})
			});
			speechSettings.set(config);
			apiKey = '';
			clearKey = false;
			saved = true;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not save speech settings.';
		} finally { saving = false; }
	}
</script>

<div class="max-w-3xl space-y-6 p-6">
	<div>
		<h1 class="text-2xl font-bold">Speech</h1>
		<p class="mt-1 text-sm text-muted-foreground">Dictate messages and read Agent replies aloud using an OpenAI-compatible audio service.</p>
	</div>
	{#if error}<p role="alert" class="rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">{error}</p>{/if}
	{#if config}
		<form onsubmit={save} oninput={() => saved = false} class="space-y-5 rounded-lg border border-border bg-card p-5">
			<fieldset disabled={saving} class="space-y-5 disabled:opacity-60">
				<label class="flex items-center gap-3 text-sm font-medium"><input type="checkbox" bind:checked={config.enabled} class="accent-primary" />Enable speech</label>
				<div class="space-y-1.5">
					<label for="speech-base-url" class="block text-sm font-medium">API base URL</label>
					<input id="speech-base-url" type="url" required bind:value={config.base_url} placeholder="https://api.openai.com/v1" class="w-full rounded-lg border border-border bg-background px-3 py-2 text-sm" />
					<p class="text-xs text-muted-foreground">Include the API prefix, usually /v1. The service must support audio transcriptions and speech generation.</p>
				</div>
				<div class="space-y-1.5">
					<label for="speech-api-key" class="block text-sm font-medium">API key</label>
					<input id="speech-api-key" type="password" autocomplete="new-password" bind:value={apiKey} disabled={clearKey} placeholder={config.has_api_key ? 'Leave blank to keep the saved key' : 'Optional for local services'} class="w-full rounded-lg border border-border bg-background px-3 py-2 text-sm" />
					<p class="text-xs text-muted-foreground">{clearKey ? 'The saved key will be removed when you save.' : config.has_api_key ? 'An API key is saved on this instance.' : 'No API key saved.'}</p>
					{#if config.has_api_key}<button type="button" onclick={() => { clearKey = !clearKey; apiKey = ''; saved = false; }} class="text-xs text-primary hover:underline">{clearKey ? 'Keep saved key' : 'Remove saved key'}</button>{/if}
				</div>
				<div class="grid gap-4 sm:grid-cols-2">
					<div class="space-y-1.5"><label for="speech-stt-model" class="block text-sm font-medium">Transcription model</label><input id="speech-stt-model" required maxlength="256" bind:value={config.stt_model} class="w-full rounded-lg border border-border bg-background px-3 py-2 text-sm" /></div>
					<div class="space-y-1.5"><label for="speech-tts-model" class="block text-sm font-medium">Speech model</label><input id="speech-tts-model" required maxlength="256" bind:value={config.tts_model} class="w-full rounded-lg border border-border bg-background px-3 py-2 text-sm" /></div>
				</div>
				<div class="space-y-1.5"><label for="speech-voice" class="block text-sm font-medium">Voice</label><input id="speech-voice" required maxlength="256" bind:value={config.voice} class="w-full rounded-lg border border-border bg-background px-3 py-2 text-sm" /><p class="text-xs text-muted-foreground">Use model and voice names supported by your provider.</p></div>
				<p class="text-xs text-muted-foreground">Recordings and text are sent to this endpoint. Dictation fills your draft for review before sending. Read aloud uses an AI-generated voice and starts only when you press play.</p>
				<div class="flex items-center gap-3"><button type="submit" class="rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">{saving ? 'Saving…' : 'Save speech settings'}</button>{#if saved}<span role="status" class="text-sm text-emerald-500">Saved</span>{/if}</div>
			</fieldset>
		</form>
	{:else if error}
		<button type="button" onclick={() => void load()} class="text-sm text-primary hover:underline">Retry</button>
	{:else}<p class="text-sm text-muted-foreground">Loading speech settings…</p>{/if}
</div>
