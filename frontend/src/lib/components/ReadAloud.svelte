<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { Volume2, Square, LoaderCircle } from '@lucide/svelte';
	import { claimSpeechPlayback, loadSpeechSettings, speechChunks, speechSettings, synthesizeSpeech } from '$lib/speech';

	let { content }: { content: string } = $props();
	let phase = $state<'idle' | 'loading' | 'playing'>('idle');
	let error = $state('');
	let request: AbortController | undefined;
	let context: AudioContext | undefined;
	let source: AudioBufferSourceNode | undefined;
	let releasePlayback: (() => void) | undefined;
	let generation = 0;

	onMount(() => { void loadSpeechSettings(); });
	onDestroy(stop);
	$effect(() => { if (!$speechSettings?.enabled) stop(); });

	function releaseAudio() {
		if (source) {
			source.onended = null;
			source.stop();
			source.disconnect();
			source = undefined;
		}
	}

	function stop() {
		generation += 1;
		request?.abort();
		request = undefined;
		releaseAudio();
		if (context) void context.close().catch(() => {});
		context = undefined;
		releasePlayback?.();
		releasePlayback = undefined;
		phase = 'idle';
	}

	function start() {
		stop();
		error = '';
		const chunks = speechChunks(content);
		if (!chunks.length) { error = 'There is no prose to read aloud in this reply.'; return; }
		let playback: AudioContext;
		let ready: Promise<void>;
		try {
			playback = new AudioContext();
			context = playback;
			// Resume during the click, before requesting audio. Safari requires
			// user activation here; all later chunks share this unlocked context.
			ready = playback.resume();
		} catch {
			stop();
			error = 'This browser could not start speech playback.';
			return;
		}
		releasePlayback = claimSpeechPlayback(stop);
		const current = generation;
		const controller = new AbortController();
		request = controller;
		async function playNext() {
			if (current !== generation) return;
			releaseAudio();
			const input = chunks.shift();
			if (!input) { stop(); return; }
			phase = 'loading';
			try {
				await ready;
				if (current !== generation) return;
				const blob = await synthesizeSpeech(input, controller.signal);
				if (current !== generation) return;
				const buffer = await playback.decodeAudioData(await blob.arrayBuffer());
				if (current !== generation) return;
				source = playback.createBufferSource();
				source.buffer = buffer;
				source.connect(playback.destination);
				source.onended = () => { void playNext(); };
				source.start();
				phase = 'playing';
			} catch (cause) {
				if (current !== generation) return;
				stop();
				error = cause instanceof Error ? cause.message : 'Speech playback failed.';
			}
		}
		void playNext();
	}
</script>

{#if $speechSettings?.enabled && content.trim()}
	<button type="button" onclick={() => phase === 'idle' ? start() : stop()}
		aria-label={phase === 'idle' ? 'Read aloud' : 'Stop reading aloud'}
		title={phase === 'idle' ? 'Read aloud with an AI-generated voice' : 'Stop reading aloud'}
		class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground">
		{#if phase === 'loading'}<LoaderCircle size={13} class="animate-spin" />
		{:else if phase === 'playing'}<Square size={13} />
		{:else}<Volume2 size={13} />{/if}
	</button>
	{#if error}<span role="alert" class="text-xs text-red-400">{error}</span>{/if}
{/if}
