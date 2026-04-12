/**
 * Headless Wanix server.
 *
 * Runs Wanix in a headless Chrome tab. The xpressclaw server communicates
 * with it via a WebSocket bridge. Each agent gets its own Wanix instance
 * (browser context).
 *
 * Usage: node wanix-server/index.mjs [--port 9100] [--xpressclaw-url http://localhost:8935]
 */

import puppeteer from 'puppeteer-core';
import { createServer } from 'http';
import { readFileSync, existsSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const STATIC_DIR = join(__dirname, '..', 'frontend', 'static', 'wanix');

// Find Chrome
const CHROME_PATHS = [
    '/usr/bin/google-chrome',
    '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
];
const chromePath = CHROME_PATHS.find(p => existsSync(p));

const PORT = parseInt(process.argv.find((_, i, a) => a[i - 1] === '--port') || '9100');
const XPRESSCLAW_URL = process.argv.find((_, i, a) => a[i - 1] === '--xpressclaw-url') || 'http://localhost:8935';

// Serve the Wanix static files so Chrome can load them
const staticServer = createServer((req, res) => {
    const url = new URL(req.url, `http://localhost:${PORT + 1}`);
    let filePath;

    if (url.pathname === '/' || url.pathname === '/index.html') {
        res.writeHead(200, { 'Content-Type': 'text/html' });
        res.end(BOOT_HTML);
        return;
    }

    // Serve wanix files
    filePath = join(STATIC_DIR, url.pathname);
    if (existsSync(filePath)) {
        const ext = filePath.split('.').pop();
        const types = { js: 'application/javascript', wasm: 'application/wasm', html: 'text/html' };
        res.writeHead(200, { 'Content-Type': types[ext] || 'application/octet-stream' });
        res.end(readFileSync(filePath));
    } else {
        res.writeHead(404);
        res.end('not found');
    }
});

const BOOT_HTML = `<!DOCTYPE html>
<html>
<head><title>Wanix Headless</title></head>
<body>
<script type="module">
import { Wanix } from './wanix.min.js';
const w = new Wanix({ helpers: true });

// Signal readiness to the controlling process
window.__wanixInstance = w;
window.__wanixReady = false;

// Poll until the kernel is ready, then mount a writable filesystem
const check = setInterval(async () => {
    if (window.wanix && window.wanix.sys) {
        clearInterval(check);
        console.log('[wanix] kernel ready, mounting writable filesystem...');

        try {
            // Allocate a tmpfs capability and mount it
            const capId = (await w.readText('cap/new/tmpfs')).trim();
            console.log('[wanix] allocated tmpfs capability:', capId);

            // Activate the capability by writing 'mount' to its ctl file
            await w.writeFile('cap/' + capId + '/ctl', 'mount');
            console.log('[wanix] mounted tmpfs capability');

            // Bind the mounted filesystem (not the cap resource) to /workspace
            await w.bind('#cap/' + capId + '/mount', 'workspace');
            console.log('[wanix] bound tmpfs mount at /workspace');

            window.__wanixReady = true;
            console.log('[wanix] ready with writable /workspace');
        } catch (e) {
            console.error('[wanix] mount failed:', e.message || e);
            window.__wanixReady = true; // boot anyway
        }
    }
}, 100);
</script>
</body>
</html>`;

async function main() {
    console.log(`Starting Wanix headless server on port ${PORT}...`);

    // Start static file server
    const staticPort = PORT + 1;
    staticServer.listen(staticPort);
    console.log(`Static server on http://localhost:${staticPort}`);

    // Launch headless Chrome
    console.log(`Using Chrome: ${chromePath}`);
    const browser = await puppeteer.launch({
        executablePath: chromePath,
        headless: true,
        args: [
            '--no-sandbox',
            '--disable-setuid-sandbox',
            '--disable-gpu',
            '--disable-dev-shm-usage',
        ],
    });

    const page = await browser.newPage();

    // Forward console output
    page.on('console', msg => {
        const type = msg.type();
        const text = msg.text();
        if (type === 'error') console.error('[chrome]', text);
        else console.log('[chrome]', text);
    });

    page.on('pageerror', err => console.error('[chrome error]', err.message));

    // Navigate to our boot page
    console.log('Loading Wanix in headless Chrome...');
    await page.goto(`http://localhost:${staticPort}/`, { waitUntil: 'domcontentloaded' });

    // Wait for Wanix kernel to be ready
    console.log('Waiting for kernel boot...');
    await page.waitForFunction('window.__wanixReady === true', { timeout: 30000 });
    console.log('Wanix kernel booted!');

    // Test filesystem
    console.log('Testing filesystem...');
    const rootDir = await page.evaluate(async () => {
        const w = window.__wanixInstance;
        try {
            const entries = await w.readDir('/');
            return { ok: true, entries };
        } catch (e) {
            return { ok: false, error: e.message || String(e) };
        }
    });
    console.log('Root directory:', JSON.stringify(rootDir, null, 2));

    // Try creating a workspace
    // Test: write, read, mkdir, list directly on workspace/
    const writeResult = await page.evaluate(async () => {
        const w = window.__wanixInstance;
        try {
            await w.writeFile('workspace/hello.txt', 'Hello from headless Wanix!');
            const content = await w.readText('workspace/hello.txt');
            await w.makeDir('workspace/src');
            await w.writeFile('workspace/src/main.py', 'print("hello world")');
            const entries = await w.readDir('workspace');
            return { ok: true, content, entries };
        } catch (e) {
            return { ok: false, error: e.message || String(e) };
        }
    });
    console.log('Filesystem test:', JSON.stringify(writeResult, null, 2));

    // List the task namespace
    const taskDir = await page.evaluate(async () => {
        const w = window.__wanixInstance;
        try {
            const entries = await w.readDir('task');
            return { ok: true, entries };
        } catch (e) {
            return { ok: false, error: e.message || String(e) };
        }
    });
    console.log('task/ directory:', JSON.stringify(taskDir, null, 2));

    // Create a WASI task
    const taskResult = await page.evaluate(async () => {
        const w = window.__wanixInstance;
        try {
            const tid = (await w.readText('task/new/wasi')).trim();
            return { ok: true, taskId: tid };
        } catch (e) {
            return { ok: false, error: e.message || String(e) };
        }
    });
    console.log('Create WASI task:', JSON.stringify(taskResult));

    // --- Tool execution API ---
    // The xpressclaw Rust server calls these endpoints to execute tools
    // in the Wanix environment.
    const apiServer = createServer(async (req, res) => {
        const url = new URL(req.url, `http://localhost:${PORT}`);

        // GET /app/{name}/{path} — serve files from the Wanix workspace
        if (req.method === 'GET' && url.pathname.startsWith('/app/')) {
            const parts = url.pathname.slice(5); // strip "/app/"
            // Try workspace/{path} first, then workspace/apps/{path}
            const filePath = 'workspace/' + parts;

            try {
                const result = await page.evaluate(async (path) => {
                    const w = window.__wanixInstance;
                    try {
                        // Try direct path first, then apps/ subdirectory
                        let stat = await w.stat(path).catch(() => null);
                        if (!stat) {
                            const altPath = path.replace('workspace/', 'workspace/apps/');
                            stat = await w.stat(altPath).catch(() => null);
                            if (stat) path = altPath;
                        }
                        if (!stat) return { status: 404 };

                        if (stat.isDir) {
                            // Try index.html
                            const indexPath = path.endsWith('/') ? path + 'index.html' : path + '/index.html';
                            try {
                                const content = await w.readText(indexPath);
                                return { status: 200, content, type: 'text/html' };
                            } catch {
                                // List directory
                                const entries = await w.readDir(path);
                                return { status: 200, content: entries.join('\n'), type: 'text/plain' };
                            }
                        }

                        const content = await w.readText(path);
                        return { status: 200, content };
                    } catch (e) {
                        return { status: 500, content: e.message || String(e) };
                    }
                }, filePath);

                if (result.status === 404) {
                    res.writeHead(404);
                    res.end('not found');
                    return;
                }

                // Detect content type from extension
                const ext = parts.split('.').pop()?.toLowerCase() || '';
                const types = {
                    html: 'text/html', css: 'text/css', js: 'application/javascript',
                    json: 'application/json', svg: 'image/svg+xml', png: 'image/png',
                    txt: 'text/plain', xml: 'application/xml',
                };
                const contentType = result.type || types[ext] || 'text/plain';

                res.writeHead(result.status, {
                    'Content-Type': contentType,
                    'Access-Control-Allow-Origin': '*',
                });
                res.end(result.content);
            } catch (e) {
                res.writeHead(500);
                res.end(String(e));
            }
            return;
        }

        if (req.method !== 'POST') {
            res.writeHead(405);
            res.end('POST only');
            return;
        }

        const body = await new Promise(resolve => {
            let data = '';
            req.on('data', chunk => data += chunk);
            req.on('end', () => resolve(data));
        });

        let request;
        try {
            request = JSON.parse(body);
        } catch {
            res.writeHead(400, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify({ error: 'invalid JSON' }));
            return;
        }

        const { tool, args } = request;

        try {
            const result = await page.evaluate(async ({ tool, args }) => {
                const w = window.__wanixInstance;
                switch (tool) {
                    case 'writeFile': {
                        await w.writeFile('workspace/' + args.path, args.content);
                        return { content: `Wrote ${args.content.length} bytes to ${args.path}` };
                    }
                    case 'readFile': {
                        const content = await w.readText('workspace/' + args.path);
                        return { content };
                    }
                    case 'listDir': {
                        const entries = await w.readDir('workspace/' + (args.path || ''));
                        return { content: entries.join('\n') };
                    }
                    case 'makeDir': {
                        await w.makeDir('workspace/' + args.path);
                        return { content: `Created directory ${args.path}` };
                    }
                    case 'remove': {
                        await w.remove('workspace/' + args.path);
                        return { content: `Removed ${args.path}` };
                    }
                    default:
                        return { content: `Unknown tool: ${tool}`, isError: true };
                }
            }, { tool, args });

            res.writeHead(200, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify(result));
        } catch (e) {
            res.writeHead(200, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify({ content: e.message || String(e), isError: true }));
        }
    });

    apiServer.listen(PORT);
    console.log(`\n=== Wanix headless server ready ===`);
    console.log(`Tool API: http://localhost:${PORT}/`);
    console.log(`Static: http://localhost:${staticPort}`);
    console.log('Press Ctrl+C to stop.\n');

    process.on('SIGINT', async () => {
        console.log('Shutting down...');
        await browser.close();
        staticServer.close();
        apiServer.close();
        process.exit(0);
    });

    await new Promise(() => {});
}

main().catch(e => {
    console.error('Fatal:', e);
    process.exit(1);
});
