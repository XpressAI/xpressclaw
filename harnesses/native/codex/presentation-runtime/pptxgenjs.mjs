import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

// PptxGenJS publishes CommonJS. This stable ESM bridge gives generated
// workspace builders one absolute import without depending on NODE_PATH or
// package-manager resolution outside the immutable runner runtime.
const PptxGenJS = require('pptxgenjs');

export default PptxGenJS;
