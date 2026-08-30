import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';

import PptxGenJS from './pptxgenjs.mjs';

const exec = promisify(execFile);
const directory = await mkdtemp(path.join(tmpdir(), 'xpressclaw-presentations-'));
try {
  const deck = new PptxGenJS();
  deck.layout = 'LAYOUT_WIDE';
  deck.author = 'XpressClaw runtime smoke test';
  deck.subject = 'Packaged presentation runtime verification';
  deck.title = 'Presentation runtime ready';
  deck.company = 'XpressAI';
  deck.lang = 'en-US';
  const slide = deck.addSlide();
  slide.background = { color: 'F7F8FC' };
  slide.addText('Presentation runtime ready', {
    x: 0.8, y: 0.75, w: 7.7, h: 0.65,
    fontFace: 'Liberation Sans', fontSize: 28, bold: true, color: '172033',
    margin: 0,
  });
  slide.addText('PptxGenJS authoring · LibreOffice rendering · Poppler previews', {
    x: 0.82, y: 1.65, w: 8.8, h: 0.4,
    fontFace: 'Liberation Sans', fontSize: 14, color: '4B5A73', margin: 0,
  });
  const output = path.join(directory, 'smoke.pptx');
  await deck.writeFile({ fileName: output });
  assert.ok((await readFile(output)).length > 1_000);
  await exec('/opt/xpressclaw/presentation-runtime/bin/xpressclaw-validate-presentation', [output], {
    timeout: 150_000,
  });
} finally {
  await rm(directory, { recursive: true, force: true });
}
