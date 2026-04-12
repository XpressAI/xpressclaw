/**
 * Test: boot Wanix headlessly in Node.
 */
import { readFileSync } from 'fs';
import { MessageChannel } from 'worker_threads';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(__dirname, '..', 'frontend', 'static', 'wanix', 'wanix.wasm');
const wasmBytes = readFileSync(wasmPath);

// --- Browser shims for headless Wanix ---

globalThis.MessageChannel = MessageChannel;
globalThis.window = globalThis;

const noop = () => {};
globalThis.document = {
    createElement(tag) {
        return {
            style: {},
            set type(v) {},
            set src(v) {},
            set textContent(v) {
                // Patch the wasm_exec to log valueCall targets
                let patched = v.replace(
                    'const m = Reflect.get(v, name);',
                    'const m = Reflect.get(v, name); if (m === undefined) console.log("[GO CALL MISS]", typeof v, name);'
                );
                try { eval(patched); } catch(e) { console.error('wasm_exec eval error:', e.message.slice(0, 200)); }
            },
            setAttribute: noop,
            hasAttribute: () => false,
            appendChild: noop,
        };
    },
    head: { appendChild: noop },
    body: { appendChild: noop, removeChild: noop },
    addEventListener: noop,
    removeEventListener: noop,
    getElementById: () => ({ style: {}, addEventListener: noop, appendChild: noop }),
    querySelectorAll: () => [],
};
globalThis.MutationObserver = class { observe() {} disconnect() {} };
// Navigator shim — the kernel needs navigator.storage.getDirectory() (OPFS)
try {
    Object.defineProperty(globalThis, 'navigator', {
        value: {
            serviceWorker: { register: async () => ({}) },
            storage: {
                async getDirectory() {
                    // Return a fake FileSystemDirectoryHandle
                    return createDirHandle('root', {});
                }
            }
        },
        writable: true,
        configurable: true,
    });
} catch {}

// Minimal FileSystemDirectoryHandle/FileSystemFileHandle shim
function createDirHandle(name, entries) {
    return {
        kind: 'directory',
        name,
        async getDirectoryHandle(childName, opts) {
            if (!entries[childName] || entries[childName].kind !== 'directory') {
                if (opts?.create) {
                    entries[childName] = { kind: 'directory', name: childName, entries: {} };
                } else {
                    throw new DOMException('Not found', 'NotFoundError');
                }
            }
            return createDirHandle(childName, entries[childName].entries);
        },
        async getFileHandle(childName, opts) {
            if (!entries[childName] || entries[childName].kind !== 'file') {
                if (opts?.create) {
                    entries[childName] = { kind: 'file', name: childName, data: new Uint8Array(0) };
                } else {
                    const err = new Error('A requested file or directory could not be found.');
                    err.name = 'NotFoundError';
                    throw err;
                }
            }
            return createFileHandle(entries[childName]);
        },
        async removeEntry(childName) { delete entries[childName]; },
        async *keys() { for (const k of Object.keys(entries)) yield k; },
        async *values() { for (const k of Object.keys(entries)) yield entries[k].kind === 'directory' ? createDirHandle(k, entries[k].entries) : createFileHandle(entries[k]); },
        async *entries() { for (const k of Object.keys(entries)) yield [k, entries[k].kind === 'directory' ? createDirHandle(k, entries[k].entries) : createFileHandle(entries[k])]; },
        [Symbol.asyncIterator]() { return this.entries(); },
    };
}

function createFileHandle(fileEntry) {
    return {
        kind: 'file',
        name: fileEntry.name,
        getFile() {
            const data = fileEntry.data;
            return Promise.resolve({
                name: fileEntry.name,
                size: data.length,
                type: '',
                lastModified: Date.now(),
                arrayBuffer: () => Promise.resolve(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength)),
                text: () => Promise.resolve(new TextDecoder().decode(data)),
                slice: () => ({ arrayBuffer: () => Promise.resolve(data.buffer.slice(0)) }),
                stream: () => new ReadableStream({ start(c) { c.enqueue(data); c.close(); } }),
            });
        },
        createWritable() {
            let chunks = [];
            return Promise.resolve({
                write(data) {
                    if (typeof data === 'string') data = new TextEncoder().encode(data);
                    if (data instanceof ArrayBuffer) data = new Uint8Array(data);
                    if (data?.data) data = data.data; // WritableStream { type, data } format
                    chunks.push(new Uint8Array(data));
                    return Promise.resolve();
                },
                close() {
                    const totalLen = chunks.reduce((s, c) => s + c.length, 0);
                    const merged = new Uint8Array(totalLen);
                    let offset = 0;
                    for (const c of chunks) { merged.set(c, offset); offset += c.length; }
                    fileEntry.data = merged;
                    return Promise.resolve();
                },
            });
        },
    };
}

globalThis.Blob = globalThis.Blob || class Blob {
    constructor(parts) { this._data = parts[0] instanceof Uint8Array ? parts[0] : new Uint8Array(0); }
    async arrayBuffer() { return this._data.buffer; }
    async text() { return new TextDecoder().decode(this._data); }
    get size() { return this._data.length; }
};

// Intercept fetch for wanix.wasm — return local file
const nodeFetch = globalThis.fetch;
globalThis.fetch = async function(url, opts) {
    const urlStr = String(url);
    if (urlStr.includes('wanix.wasm') || urlStr === './wanix.wasm') {
        return {
            ok: true,
            arrayBuffer: async () => wasmBytes.buffer.slice(0),
            body: null,
        };
    }
    return nodeFetch(url, opts);
};

// Trap missing properties on globalThis to find what the kernel needs
const missingProps = new Set();
const origGet = Object.getOwnPropertyDescriptor(Object.prototype, '__lookupGetter__');
const globalProxy = new Proxy(globalThis, {
    get(target, prop) {
        const val = Reflect.get(target, prop);
        if (val === undefined && typeof prop === 'string' && !prop.startsWith('_') && prop !== 'constructor') {
            if (!missingProps.has(prop)) {
                missingProps.add(prop);
                console.log('[SHIM MISS]', prop);
            }
        }
        return val;
    }
});
// Can't replace globalThis, but we can shim common things the kernel needs
// Node already has console, performance, crypto — just ensure they exist

// --- Boot ---

console.log('Loading Wanix module...');
const { Wanix } = await import(join(__dirname, '..', 'frontend', 'static', 'wanix', 'wanix.min.js'));

console.log('Creating Wanix instance...');
const w = new Wanix({});

console.log('Waiting for kernel boot (8s)...');
await new Promise(r => setTimeout(r, 8000));
console.log('Boot wait done. wanix global:', typeof globalThis.wanix);
console.log('wanix.sys:', typeof globalThis.wanix?.sys);

// Test filesystem
try {
    console.log('Testing filesystem...');

    console.log('  readDir /...');
    const rootEntries = await w.readDir('/').catch(e => 'ERR:' + e);
    console.log('  root:', JSON.stringify(rootEntries));

    // Try writing to paths that might already be writable
    console.log('  writeFile /tmp/test.txt...');
    const wrResult = await w.writeFile('/tmp/test.txt', new TextEncoder().encode('hello')).catch(e => 'ERR:' + e);
    console.log('  writeFile result:', wrResult);

    console.log('  readText /tmp/test.txt...');
    const content = await w.readText('/tmp/test.txt').catch(e => 'ERR:' + e);
    console.log('  readText result:', JSON.stringify(content));

    if (typeof content === 'string' && content.includes('Hello')) {
        console.log('\n=== SUCCESS: Wanix boots headlessly in Node! ===');
    } else {
        console.log('\n=== PARTIAL: Wanix boots but filesystem needs work ===');
    }
} catch (e) {
    console.error('Filesystem test failed:', e?.message || e);
    console.error(e?.stack);
}

process.exit(0);
