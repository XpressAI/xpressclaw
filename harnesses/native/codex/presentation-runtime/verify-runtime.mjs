import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
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
  // Keep the mixed-case suffix: validation includes rendering, so this also
  // verifies that LibreOffice's lowercase PDF output is found correctly.
  const output = path.join(directory, 'smoke.PPTX');
  await deck.writeFile({ fileName: output });
  assert.ok((await readFile(output)).length > 1_000);
  await exec('/opt/xpressclaw/presentation-runtime/bin/xpressclaw-validate-presentation', [output], {
    timeout: 150_000,
  });

  const oversized = Buffer.from(await readFile(output));
  const centralHeader = oversized.indexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02]));
  assert.ok(centralHeader >= 0, 'smoke deck should contain a ZIP central directory');
  oversized.writeUInt32LE(129 * 1024 * 1024, centralHeader + 24);
  const oversizedOutput = path.join(directory, 'oversized-uncompressed.pptx');
  await writeFile(oversizedOutput, oversized);
  await assert.rejects(
    exec('/opt/xpressclaw/presentation-runtime/bin/xpressclaw-validate-presentation', [oversizedOutput], {
      timeout: 10_000,
    }),
    /uncompressed limit/,
  );

  const fakeBin = path.join(directory, 'fake-bin');
  await mkdir(fakeBin);
  await writeFile(path.join(fakeBin, 'libreoffice'), `#!/usr/bin/env bash
set -euo pipefail
outdir=""
input=""
while (( $# )); do
  case "$1" in
    --outdir) outdir="$2"; shift 2 ;;
    *) input="$1"; shift ;;
  esac
done
mkdir -p "$outdir"
printf 'bounded preview fixture' >"$outdir/$(basename "\${input%.*}").pdf"
`);
  await writeFile(path.join(fakeBin, 'pdfinfo'), `#!/usr/bin/env bash
set -euo pipefail
case "\${XPRESSCLAW_TEST_PDFINFO:-valid}" in
  pages) printf 'Pages: 201\nPage size: 720 x 405 pts\n' ;;
  dimensions) printf 'Pages: 1\nPage size: 2000 x 400 pts\n' ;;
  pixels) printf 'Pages: 100\nPage size: 1000 x 1000 pts\n' ;;
  *) printf 'Pages: 1\nPage size: 720 x 405 pts\n' ;;
esac
`);
  await writeFile(path.join(fakeBin, 'pdftoppm'), `#!/usr/bin/env bash
echo 'pdftoppm must not run after an unsafe preview is rejected' >&2
exit 99
`);
  await Promise.all(['libreoffice', 'pdfinfo', 'pdftoppm'].map(
    (name) => chmod(path.join(fakeBin, name), 0o755),
  ));
  const render = '/opt/xpressclaw/presentation-runtime/bin/xpressclaw-render-slides';
  const limitInput = path.join(directory, 'limit-fixture.PPTX');
  await writeFile(limitInput, 'fixture');
  const limitCases = [
    ['pages', /maximum is 200/],
    ['dimensions', /exceed the 1440-point limit/],
    ['pixels', /maximum is 150000000/],
  ];
  for (const [mode, expected] of limitCases) {
    await assert.rejects(
      exec(render, [limitInput, path.join(directory, `render-${mode}`)], {
        env: {
          ...process.env,
          PATH: `${fakeBin}:${process.env.PATH}`,
          XPRESSCLAW_TEST_PDFINFO: mode,
        },
        timeout: 10_000,
      }),
      expected,
    );
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}
