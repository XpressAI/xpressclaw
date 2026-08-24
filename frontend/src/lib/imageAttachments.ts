import type { ImageAttachmentUpload } from '$lib/api';
import { isTauri } from '@tauri-apps/api/core';
import { readImage } from '@tauri-apps/plugin-clipboard-manager';

export const MAX_IMAGE_ATTACHMENTS = 5;
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
export const MAX_TOTAL_IMAGE_BYTES = 20 * 1024 * 1024;
export const IMAGE_FILE_ACCEPT = 'image/png,image/jpeg,image/gif,image/webp';

const allowedTypes = new Set(IMAGE_FILE_ACCEPT.split(','));

export async function appendImageFiles(
	current: ImageAttachmentUpload[],
	files: File[],
): Promise<ImageAttachmentUpload[]> {
	if (files.length === 0) return current;
	if (current.length + files.length > MAX_IMAGE_ATTACHMENTS) {
		throw new Error(`You can attach up to ${MAX_IMAGE_ATTACHMENTS} images.`);
	}
	for (const file of files) {
		if (!allowedTypes.has(file.type)) {
			throw new Error(`${file.name || 'This file'} is not a PNG, JPEG, GIF, or WebP image.`);
		}
		if (file.size > MAX_IMAGE_BYTES) {
			throw new Error(`${file.name || 'This image'} is larger than 5 MiB.`);
		}
	}
	const currentSize = current.reduce((total, attachment) => total + decodedSize(attachment.data), 0);
	const addedSize = files.reduce((total, file) => total + file.size, 0);
	if (currentSize + addedSize > MAX_TOTAL_IMAGE_BYTES) {
		throw new Error('Images in one message cannot exceed 20 MiB in total.');
	}

	const additions = await Promise.all(files.map(async (file): Promise<ImageAttachmentUpload> => ({
		name: file.name || `pasted-image.${extensionFor(file.type)}`,
		mime_type: file.type,
		data: await fileAsBase64(file),
	})));
	return [...current, ...additions];
}

export function clipboardFiles(event: ClipboardEvent): File[] {
	const clipboard = event.clipboardData;
	if (!clipboard) return [];

	const files = Array.from(clipboard.files);
	if (files.length > 0) return files;

	return Array.from(clipboard.items)
		.filter((item) => item.kind === 'file')
		.map((item) => item.getAsFile())
		.filter((file): file is File => file !== null);
}

export function clipboardImageFiles(event: ClipboardEvent): File[] {
	return clipboardFiles(event).filter((file) => file.type.startsWith('image/'));
}

export function shouldHandleImagePaste(event: ClipboardEvent): boolean {
	if (clipboardImageFiles(event).length > 0) return true;
	if (!isTauri()) return false;

	const types = Array.from(event.clipboardData?.types ?? []);
	return types.length === 0 || types.some((type) => type === 'Files' || type.startsWith('image/'));
}

export async function pastedImageFiles(event: ClipboardEvent): Promise<File[]> {
	const browserFiles = clipboardImageFiles(event);
	if (browserFiles.length > 0) return browserFiles;
	if (!shouldHandleImagePaste(event)) return [];

	let image;
	try {
		image = await readImage();
	} catch {
		throw new Error('Could not read an image from the system clipboard.');
	}

	try {
		const [rgba, size] = await Promise.all([image.rgba(), image.size()]);
		return [await rgbaImageFile(rgba, size.width, size.height)];
	} finally {
		await image.close().catch(() => undefined);
	}
}

export function imageDataUrl(attachment: Pick<ImageAttachmentUpload, 'mime_type' | 'data'>): string {
	return `data:${attachment.mime_type};base64,${attachment.data}`;
}

function fileAsBase64(file: File): Promise<string> {
	return new Promise((resolve, reject) => {
		const reader = new FileReader();
		reader.onerror = () => reject(new Error(`Could not read ${file.name || 'the image'}.`));
		reader.onload = () => {
			const value = typeof reader.result === 'string' ? reader.result : '';
			const separator = value.indexOf(',');
			if (separator < 0) {
				reject(new Error(`Could not encode ${file.name || 'the image'}.`));
				return;
			}
			resolve(value.slice(separator + 1));
		};
		reader.readAsDataURL(file);
	});
}

function decodedSize(data: string): number {
	const padding = data.endsWith('==') ? 2 : data.endsWith('=') ? 1 : 0;
	return Math.max(0, Math.floor(data.length * 3 / 4) - padding);
}

function extensionFor(mimeType: string): string {
	if (mimeType === 'image/jpeg') return 'jpg';
	return mimeType.slice('image/'.length) || 'png';
}

async function rgbaImageFile(rgba: Uint8Array, width: number, height: number): Promise<File> {
	const expectedLength = width * height * 4;
	if (!Number.isSafeInteger(expectedLength) || width <= 0 || height <= 0 || rgba.length !== expectedLength) {
		throw new Error('The clipboard returned invalid image data.');
	}

	const canvas = document.createElement('canvas');
	canvas.width = width;
	canvas.height = height;
	const context = canvas.getContext('2d');
	if (!context) throw new Error('Could not prepare the clipboard image.');

	context.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
	const blob = await new Promise<Blob>((resolve, reject) => {
		canvas.toBlob((result) => {
			if (result) resolve(result);
			else reject(new Error('Could not encode the clipboard image.'));
		}, 'image/png');
	});
	return new File([blob], 'pasted-image.png', { type: 'image/png' });
}
