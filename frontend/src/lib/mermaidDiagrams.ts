import { tick } from 'svelte';
import type { Action } from 'svelte/action';
import DOMPurify from 'dompurify';

interface DiagramImage {
	src: string;
	width: number;
	height: number;
}

interface Diagram {
	source: string;
	dark: boolean;
	image?: Promise<DiagramImage | null>;
}

let nextId = 0;
let renderQueue = Promise.resolve();

function renderDiagram(diagram: Diagram, isCurrent: () => boolean): Promise<DiagramImage | null> {
	// Mermaid has global configuration. Serialize initialization and rendering so
	// simultaneous messages and theme changes cannot change one another's theme.
	const render = renderQueue.then(async () => {
		if (!isCurrent() || diagram.source.length > 50_000) return null;
		const { default: mermaid } = await import('mermaid');
		if (!isCurrent()) return null;
		mermaid.initialize({
			startOnLoad: false,
			securityLevel: 'strict',
			suppressErrorRendering: true,
			theme: diagram.dark ? 'dark' : 'default',
			htmlLabels: false,
			fontFamily: 'sans-serif',
			maxTextSize: 50_000,
			maxEdges: 500,
			secure: [
				'secure', 'securityLevel', 'startOnLoad', 'suppressErrorRendering',
				'maxTextSize', 'maxEdges', 'htmlLabels', 'fontFamily',
				'theme', 'themeCSS', 'themeVariables', 'dompurifyConfig',
			],
		});
		// Layout needs a connected DOM with measurable text, including when the
		// message is in a collapsed details element or an inactive workspace tab.
		const staging = document.createElement('div');
		staging.style.cssText = 'position:fixed;left:-100000px;top:0;visibility:hidden;pointer-events:none';
		staging.setAttribute('aria-hidden', 'true');
		document.body.append(staging);
		try {
			const { svg } = await mermaid.render(`message-diagram-${++nextId}`, diagram.source, staging);
			const sanitized = DOMPurify.sanitize(svg, {
				USE_PROFILES: { svg: true, svgFilters: true },
				FORBID_TAGS: ['foreignObject', 'a', 'image'],
			});
			const svgDocument = new DOMParser().parseFromString(sanitized, 'image/svg+xml');
			const root = svgDocument.documentElement;
			const [, , width, height] = (root.getAttribute('viewBox') ?? '').split(/[\s,]+/).map(Number);
			if (root.tagName !== 'svg' || !(width > 0) || !(height > 0)) return null;
			root.setAttribute('width', String(width));
			root.setAttribute('height', String(height));
			// An SVG image isolates diagram styles from the application and keeps
			// generated links and scripts inert. The source remains selectable below.
			return {
				src: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(new XMLSerializer().serializeToString(root))}`,
				width,
				height,
			};
		} finally {
			staging.remove();
		}
	}).catch(() => null);
	// Invalid or still-streaming syntax must not block subsequent diagrams.
	renderQueue = render.then(() => {});
	return render;
}

/** Enhance sanitized Markdown code fences without changing the stored message. */
export const mermaidDiagrams: Action<HTMLElement, string> = (node) => {
	let diagrams: Diagram[] = [];
	let revision = 0;
	let destroyed = false;
	let timer: ReturnType<typeof setTimeout>;
	let dark = document.documentElement.classList.contains('dark');

	async function render(version: number) {
		await tick();
		if (destroyed || version !== revision) return;
		const codes = [...node.querySelectorAll<HTMLElement>('pre > code')].filter((code) =>
			[...code.classList].some((name) => name.toLowerCase() === 'language-mermaid'));
		diagrams = codes.map((code, index) => {
			const source = code.textContent ?? '';
			const previous = diagrams[index];
			return previous?.source === source && previous.dark === dark ? previous : { source, dark };
		});
		for (const [index, code] of codes.entries()) {
			const diagram = diagrams[index];
			diagram.image ??= renderDiagram(diagram, () => !destroyed && diagrams.includes(diagram));
			void diagram.image.then((image) => {
				if (!image || destroyed || version !== revision || !node.contains(code)) return;
				const pre = code.parentElement!;
				const previous = pre.closest<HTMLElement>('.mermaid-diagram');
				const figure = document.createElement('figure');
				figure.className = 'mermaid-diagram not-prose';
				const viewport = document.createElement('div');
				viewport.className = 'mermaid-diagram-viewport';
				viewport.dataset.actualSize = previous?.querySelector<HTMLElement>('.mermaid-diagram-viewport')?.dataset.actualSize ?? 'false';
				viewport.tabIndex = 0;
				viewport.setAttribute('role', 'region');
				viewport.setAttribute('aria-label', 'Mermaid diagram');
				const img = document.createElement('img');
				img.alt = 'Mermaid diagram';
				img.src = image.src;
				img.width = Math.ceil(image.width);
				img.height = Math.ceil(image.height);
				viewport.append(img);
				const controls = document.createElement('div');
				controls.className = 'mermaid-diagram-controls';
				const sizeButton = document.createElement('button');
				sizeButton.type = 'button';
				const updateSizeLabel = () => {
					sizeButton.textContent = viewport.dataset.actualSize === 'true' ? 'Fit diagram' : 'Actual size';
				};
				sizeButton.addEventListener('click', () => {
					viewport.dataset.actualSize = String(viewport.dataset.actualSize !== 'true');
					updateSizeLabel();
				});
				updateSizeLabel();
				controls.append(sizeButton);
				const details = document.createElement('details');
				details.open = previous?.querySelector('details')?.open ?? false;
				const summary = document.createElement('summary');
				summary.textContent = 'Diagram source';
				details.append(summary);
				figure.append(controls, viewport, details);
				(previous ?? pre).replaceWith(figure);
				details.append(pre);
			});
		}
	}

	function schedule() {
		const version = ++revision;
		clearTimeout(timer);
		// Coalesce streamed chunks; completed blocks reuse their previous render.
		timer = setTimeout(() => void render(version), 150);
	}
	const themeObserver = new MutationObserver(() => {
		const nextDark = document.documentElement.classList.contains('dark');
		if (nextDark === dark) return;
		dark = nextDark;
		schedule();
	});
	themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
	schedule();
	return {
		update: schedule,
		destroy() {
			destroyed = true;
			clearTimeout(timer);
			themeObserver.disconnect();
			diagrams = [];
		},
	};
};
