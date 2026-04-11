/**
 * Bridge between SvelteKit and a Wanix instance.
 *
 * Manages the Wanix lifecycle: boot, filesystem operations, task
 * creation, and communication with the xpressclaw server.
 *
 * Wanix only allows one instance per page (enforced by the library).
 * We treat it as a singleton managed by the active agent's workspace.
 */

/** Minimal type for the Wanix JS API (from wanix.min.js). */
export interface WanixInstance {
	readText(path: string): Promise<string>;
	readFile(path: string): Promise<Uint8Array>;
	readDir(path: string): Promise<Array<{ name: string; size: number; isDir: boolean }>>;
	writeFile(path: string, contents: string | Uint8Array): Promise<void>;
	appendFile(path: string, contents: string | Uint8Array): Promise<void>;
	makeDir(path: string): Promise<void>;
	remove(path: string): Promise<void>;
	stat(path: string): Promise<{ name: string; size: number; isDir: boolean }>;
	bind(source: string, target: string): Promise<void>;
	unbind(source: string, target: string): Promise<void>;
	open(path: string): Promise<number>;
	read(fd: number, count: number): Promise<Uint8Array | null>;
	write(fd: number, data: Uint8Array): Promise<void>;
	close(fd: number): Promise<void>;
	openReadable(path: string): Promise<ReadableStream<Uint8Array>>;
	openWritable(path: string): Promise<WritableStream<Uint8Array>>;
}

let instance: WanixInstance | null = null;
let booting = false;

/**
 * Boot a Wanix instance.  Resolves when the kernel is ready.
 * Returns the existing instance if already booted.
 */
export async function boot(): Promise<WanixInstance> {
	if (instance) return instance;
	if (booting) {
		// Wait for the in-progress boot
		return new Promise((resolve) => {
			const check = setInterval(() => {
				if (instance) {
					clearInterval(check);
					resolve(instance);
				}
			}, 100);
		});
	}

	booting = true;

	// Wanix sets window.wanix.instance after init.  We load the script
	// dynamically so it can find its wanix.wasm relative to /wanix/.
	const script = document.createElement('script');
	script.type = 'module';
	script.textContent = `
		import { Wanix } from '/wanix/wanix.min.js';
		window.__wanixReady = new Wanix({ helpers: true });
	`;
	document.head.appendChild(script);

	// Wait for wanix.instance to be populated
	return new Promise((resolve, reject) => {
		const timeout = setTimeout(() => {
			booting = false;
			reject(new Error('Wanix boot timed out after 30s'));
		}, 30_000);

		const check = setInterval(() => {
			const w = (window as any).wanix?.instance;
			if (w) {
				clearInterval(check);
				clearTimeout(timeout);
				instance = w as WanixInstance;
				booting = false;
				console.log('[wanix-bridge] Wanix booted');
				resolve(instance);
			}
		}, 100);
	});
}

/** Get the current Wanix instance (null if not booted). */
export function getInstance(): WanixInstance | null {
	return instance;
}

/** Shutdown the Wanix instance. */
export function shutdown(): void {
	instance = null;
	// Wanix doesn't expose a clean shutdown — the GC will handle it
	// when the page navigates away.
	delete (window as any).wanix;
	delete (window as any).__wanixReady;
}

/**
 * Create a WASI task in Wanix and return its task ID.
 */
export async function createTask(w: WanixInstance): Promise<string> {
	return (await w.readText('task/new/wasi')).trim();
}

/**
 * Start a task with a given command.
 */
export async function startTask(
	w: WanixInstance,
	taskId: string,
	cmd: string,
	args: string[] = [],
	env: Record<string, string> = {},
	cwd = '/workspace'
): Promise<void> {
	await w.writeFile(`task/${taskId}/cmd`, cmd);
	if (args.length) {
		await w.writeFile(`task/${taskId}/args`, args.join('\n'));
	}
	const envStr = Object.entries(env)
		.map(([k, v]) => `${k}=${v}`)
		.join('\n');
	if (envStr) {
		await w.writeFile(`task/${taskId}/env`, envStr);
	}
	await w.writeFile(`task/${taskId}/cwd`, cwd);
	await w.writeFile(`task/${taskId}/ctl`, 'start');
}

/**
 * Read stdout from a task as a ReadableStream.
 */
export async function taskStdout(
	w: WanixInstance,
	taskId: string
): Promise<ReadableStream<Uint8Array>> {
	return w.openReadable(`task/${taskId}/fd/1`);
}
