import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		warmup: {
			clientFiles: ['./src/routes/**/*.svelte', './src/lib/**/*.svelte']
		},
		proxy: {
			'/api': 'http://localhost:8935',
			'/v1': 'http://localhost:8935'
		}
	}
});
