<script lang="ts">
	import { onMount } from 'svelte';
	import { settings } from '$lib/api';
	import {
		getThemePreference,
		setThemePreference,
		type ThemePreference,
	} from '$lib/theme';
	import { setCachedProfile } from '$lib/utils';

	import { health } from '$lib/api';

	let userProfile = $state<{ name: string; avatar: string | null }>({ name: 'You', avatar: null });
	let editingProfile = $state(false);
	let buildInfo = $state<{ version: string; build: string; git_hash: string } | null>(null);
	let profileName = $state('');
	let profileSaved = $state(false);
	let selectedTheme = $state<ThemePreference>('system');
	let fileInput: HTMLInputElement;
	const themeOptions: {
		value: ThemePreference;
		label: string;
		description: string;
		icon: 'system' | 'light' | 'dark';
	}[] = [
		{ value: 'system', label: 'System', description: 'Match this device', icon: 'system' },
		{ value: 'light', label: 'Light', description: 'Use the light theme', icon: 'light' },
		{ value: 'dark', label: 'Dark', description: 'Use the dark theme', icon: 'dark' },
	];

	function startEditProfile() {
		profileName = userProfile.name;
		editingProfile = true;
	}

	async function saveProfile() {
		userProfile = { ...userProfile, name: profileName.trim() || 'You' };
		await persistProfile();
		editingProfile = false;
	}

	function triggerUpload() {
		fileInput?.click();
	}

	async function handleFileUpload(e: Event) {
		const target = e.target as HTMLInputElement;
		const file = target.files?.[0];
		if (!file) return;

		// Resize to 128x128 and convert to data URI
		const dataUri = await resizeImage(file, 128);
		userProfile = { ...userProfile, avatar: dataUri };
		await persistProfile();
		target.value = '';
	}

	function resizeImage(file: File, size: number): Promise<string> {
		return new Promise((resolve) => {
			const reader = new FileReader();
			reader.onload = () => {
				const img = new Image();
				img.onload = () => {
					const canvas = document.createElement('canvas');
					canvas.width = size;
					canvas.height = size;
					const ctx = canvas.getContext('2d')!;

					// Crop to square from center
					const min = Math.min(img.width, img.height);
					const sx = (img.width - min) / 2;
					const sy = (img.height - min) / 2;
					ctx.drawImage(img, sx, sy, min, min, 0, 0, size, size);

					resolve(canvas.toDataURL('image/jpeg', 0.85));
				};
				img.src = reader.result as string;
			};
			reader.readAsDataURL(file);
		});
	}

	async function removeAvatar() {
		userProfile = { ...userProfile, avatar: null };
		await persistProfile();
	}

	async function persistProfile() {
		try {
			await settings.putProfile(userProfile);
			setCachedProfile(userProfile);
			profileSaved = true;
			setTimeout(() => (profileSaved = false), 2000);
		} catch {}
	}

	function chooseTheme(preference: ThemePreference) {
		selectedTheme = preference;
		setThemePreference(preference);
	}

	onMount(async () => {
		selectedTheme = getThemePreference();
		const [profile, info] = await Promise.all([
			settings.getProfile().catch(() => null),
			health.check().catch(() => null),
		]);
		if (profile) {
			userProfile = profile;
			setCachedProfile(profile);
		}
		if (info) {
			buildInfo = { version: info.version, build: info.build, git_hash: info.git_hash };
		}
	});
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Profile</h1>
		<p class="text-sm text-muted-foreground mt-1">Your identity in task chat</p>
	</div>

	<!-- User Profile -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-4">
		<div class="flex justify-between items-center">
			<h2 class="text-sm font-semibold">Your Profile</h2>
			{#if profileSaved}
				<span class="text-xs text-emerald-400">Saved</span>
			{/if}
		</div>

		<input type="file" accept="image/*" bind:this={fileInput} onchange={handleFileUpload} class="hidden" />

		<div class="flex items-center gap-4">
			<!-- Avatar -->
			<button onclick={triggerUpload} class="relative group flex-shrink-0" title="Upload picture">
				{#if userProfile.avatar}
					<img src={userProfile.avatar} alt="" class="h-14 w-14 rounded-full object-cover" />
				{:else}
					<div class="h-14 w-14 rounded-full flex items-center justify-center text-lg font-bold bg-primary/20 text-primary">
						{userProfile.name[0].toUpperCase()}
					</div>
				{/if}
				<div class="absolute inset-0 rounded-full bg-black/40 flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity">
					<svg class="h-5 w-5 text-white" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6.827 6.175A2.31 2.31 0 015.186 7.23c-.38.054-.757.112-1.134.175C2.999 7.58 2.25 8.507 2.25 9.574V18a2.25 2.25 0 002.25 2.25h15A2.25 2.25 0 0021.75 18V9.574c0-1.067-.75-1.994-1.802-2.169a47.865 47.865 0 00-1.134-.175 2.31 2.31 0 01-1.64-1.055l-.822-1.316a2.192 2.192 0 00-1.736-1.039 48.774 48.774 0 00-5.232 0 2.192 2.192 0 00-1.736 1.039l-.821 1.316z" /><path stroke-linecap="round" stroke-linejoin="round" d="M16.5 12.75a4.5 4.5 0 11-9 0 4.5 4.5 0 019 0z" /></svg>
				</div>
			</button>

			<!-- Name -->
			<div class="flex-1">
				{#if editingProfile}
					<form onsubmit={(e) => { e.preventDefault(); saveProfile(); }} class="flex items-center gap-2">
						<input
							type="text"
							bind:value={profileName}
							placeholder="Your name"
							class="flex-1 rounded-lg border border-border bg-secondary px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
						/>
						<button type="submit" class="text-xs text-primary hover:underline">Save</button>
						<button type="button" onclick={() => (editingProfile = false)} class="text-xs text-muted-foreground hover:underline">Cancel</button>
					</form>
				{:else}
					<div class="flex items-center gap-2">
						<span class="text-sm font-medium">{userProfile.name}</span>
						<button onclick={startEditProfile} class="text-xs text-muted-foreground hover:text-foreground transition-colors">Edit</button>
					</div>
					<div class="flex items-center gap-2 mt-0.5">
						<p class="text-xs text-muted-foreground">Shown beside your messages</p>
						{#if userProfile.avatar}
							<button onclick={removeAvatar} class="text-xs text-muted-foreground hover:text-destructive transition-colors">Remove picture</button>
						{/if}
					</div>
				{/if}
			</div>
		</div>
	</div>

	<!-- Appearance -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-4">
		<div>
			<h2 class="text-sm font-semibold">Appearance</h2>
			<p class="mt-1 text-xs text-muted-foreground">Choose how XpressClaw looks on this device.</p>
		</div>
		<div class="grid gap-2 sm:grid-cols-3" role="radiogroup" aria-label="Appearance">
			{#each themeOptions as option (option.value)}
				<label
					data-theme-option={option.value}
					class="flex cursor-pointer items-center gap-3 rounded-lg border p-3 transition-colors
						{selectedTheme === option.value
							? 'border-primary bg-primary/5 ring-1 ring-primary/20'
							: 'border-border hover:bg-accent/50'}"
				>
					<input
						class="sr-only"
						type="radio"
						name="appearance"
						value={option.value}
						checked={selectedTheme === option.value}
						onchange={() => chooseTheme(option.value)}
					/>
					<span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-md border border-border bg-background text-foreground">
						{#if option.icon === 'system'}
							<svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.6" viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="4" width="18" height="13" rx="2"/><path stroke-linecap="round" d="M8 21h8m-4-4v4"/></svg>
						{:else if option.icon === 'light'}
							<svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.6" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="3.5"/><path stroke-linecap="round" d="M12 2v2m0 16v2M4.93 4.93l1.42 1.42m11.3 11.3 1.42 1.42M2 12h2m16 0h2M4.93 19.07l1.42-1.42m11.3-11.3 1.42-1.42"/></svg>
						{:else}
							<svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.6" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M20.5 15.3A8.5 8.5 0 0 1 8.7 3.5a8.5 8.5 0 1 0 11.8 11.8Z"/></svg>
						{/if}
					</span>
					<span class="min-w-0">
						<span class="block text-sm font-medium">{option.label}</span>
						<span class="block text-xs text-muted-foreground">{option.description}</span>
					</span>
				</label>
			{/each}
		</div>
	</div>

	<!-- Build info -->
	{#if buildInfo}
		<div class="rounded-lg border border-border bg-card p-4">
			<h2 class="text-sm font-semibold mb-2">About</h2>
			<div class="text-xs text-muted-foreground space-y-1">
				<div class="flex gap-2">
					<span>Version:</span>
					<span class="font-mono text-foreground">{buildInfo.version}</span>
				</div>
				<div class="flex gap-2">
					<span>Build:</span>
					<span class="font-mono text-foreground">{buildInfo.build}</span>
				</div>
				<div class="flex gap-2">
					<span>Commit:</span>
					<span class="font-mono text-foreground">{buildInfo.git_hash}</span>
				</div>
			</div>
		</div>
	{/if}
</div>
