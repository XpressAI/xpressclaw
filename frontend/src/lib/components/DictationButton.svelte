<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { Mic, Square, X, LoaderCircle } from '@lucide/svelte';
	import { loadSpeechSettings, speechSettings, transcribeAudio } from '$lib/speech';

	let { disabled = false, ontranscript }: { disabled?: boolean; ontranscript: (text: string) => void } = $props();
	let phase = $state<'idle' | 'permission' | 'recording' | 'transcribing'>('idle');
	let error = $state('');
	let supported = $state(false);
	let recorder: MediaRecorder | undefined;
	let stream: MediaStream | undefined;
	let request: AbortController | undefined;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let generation = 0;

	onMount(() => {
		supported = !!navigator.mediaDevices?.getUserMedia && typeof MediaRecorder !== 'undefined';
		void loadSpeechSettings();
	});
	onDestroy(cancel);
	$effect(() => { if (disabled || !$speechSettings?.enabled) cancel(); });

	function releaseMicrophone() {
		clearTimeout(timer);
		stream?.getTracks().forEach((track) => track.stop());
		stream = undefined;
	}

	function cancel() {
		generation += 1;
		request?.abort();
		request = undefined;
		if (recorder && recorder.state !== 'inactive') recorder.stop();
		recorder = undefined;
		releaseMicrophone();
		phase = 'idle';
	}

	function finish() {
		if (recorder?.state === 'recording') recorder.stop();
		releaseMicrophone();
	}

	async function start() {
		error = '';
		if (!supported) {
			error = 'Microphone recording requires a supported browser on HTTPS or localhost.';
			return;
		}
		const current = ++generation;
		phase = 'permission';
		try {
			const acquired = await navigator.mediaDevices.getUserMedia({ audio: true });
			if (current !== generation) {
				acquired.getTracks().forEach((track) => track.stop());
				return;
			}
			stream = acquired;
			const mimeType = ['audio/webm;codecs=opus', 'audio/mp4', 'audio/ogg;codecs=opus']
				.find((type) => MediaRecorder.isTypeSupported(type));
			if (!mimeType) throw new Error('This browser cannot record a supported audio format.');
			const recording = new MediaRecorder(acquired, { mimeType });
			recorder = recording;
			const chunks: Blob[] = [];
			let size = 0;
			recording.ondataavailable = (event) => {
				if (current !== generation) return;
				size += event.data.size;
				if (size > 25 * 1024 * 1024) {
					cancel();
					error = 'Recording exceeded 25 MiB. Please try a shorter message.';
					return;
				}
				chunks.push(event.data);
			};
			recording.onerror = () => {
				if (current !== generation) return;
				cancel();
				error = 'Microphone recording failed. Please try again.';
			};
			recording.onstop = async () => {
				if (current !== generation) return;
				recorder = undefined;
				releaseMicrophone();
				phase = 'transcribing';
				request = new AbortController();
				try {
					const audio = new Blob(chunks, { type: recording.mimeType });
					if (!audio.size) throw new Error('No audio was recorded. Please try again.');
					const transcript = await transcribeAudio(audio, request.signal);
					if (current !== generation) return;
					if (!transcript) throw new Error('No speech was detected. Please try again.');
					ontranscript(transcript);
				} catch (cause) {
					if (current === generation) error = cause instanceof Error ? cause.message : 'Transcription failed.';
				} finally {
					if (current === generation) { phase = 'idle'; request = undefined; }
				}
			};
			recording.start(1000);
			phase = 'recording';
			timer = setTimeout(finish, 5 * 60 * 1000);
		} catch (cause) {
			if (current !== generation) return;
			cancel();
			error = cause instanceof DOMException && cause.name === 'NotAllowedError'
				? 'Microphone access was denied. Allow access in your browser or system settings to dictate.'
				: cause instanceof Error ? cause.message : 'Could not access the microphone.';
		}
	}
</script>

{#if $speechSettings?.enabled}
	<div class="relative flex items-center gap-1">
		<button
			type="button"
			onclick={() => phase === 'recording' ? finish() : start()}
			disabled={disabled || phase === 'permission' || phase === 'transcribing'}
			aria-label={phase === 'recording' ? 'Finish dictation' : 'Dictate message'}
			title={phase === 'recording' ? 'Finish and transcribe (5 minute limit)' : 'Dictate a message'}
			class="flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
		>
			{#if phase === 'recording'}<Square size={15} class="text-red-400" />
			{:else if phase === 'permission' || phase === 'transcribing'}<LoaderCircle size={15} class="animate-spin" />
			{:else}<Mic size={16} />{/if}
		</button>
		{#if phase !== 'idle'}
			<span class="text-xs text-muted-foreground" role="status">{phase === 'recording' ? 'Recording…' : phase === 'permission' ? 'Microphone…' : 'Transcribing…'}</span>
			<button type="button" onclick={cancel} aria-label="Cancel dictation" title="Cancel dictation" class="rounded p-1 text-muted-foreground hover:text-foreground"><X size={14} /></button>
		{/if}
		{#if error}
			<div role="alert" class="absolute bottom-full left-0 z-20 mb-2 flex w-72 gap-2 rounded-lg border border-border bg-card p-3 text-xs text-destructive shadow-lg">
				<span>{error}</span><button type="button" onclick={() => error = ''} aria-label="Dismiss dictation error" class="self-start"><X size={14} /></button>
			</div>
		{/if}
	</div>
{/if}
