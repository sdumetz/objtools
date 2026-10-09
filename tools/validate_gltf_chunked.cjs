// Validate objtools output with Khronos without loading the whole binary payload.
// NODE_PATH=/tmp/objtools-gltf-validation/node_modules node tools/validate_gltf_chunked.cjs file.gltf [report.json]
const fs = require('node:fs');
const path = require('node:path');
const validator = require('gltf-validator');
(async () => {
  const filename = process.argv[2];
  if (!filename) throw new Error('Expected a glTF filename');
  const sourceText = fs.readFileSync(filename, 'utf8');
  const root = JSON.parse(sourceText);
  const directory = path.dirname(filename);
  const report = { file: filename, validator: validator.version(), metadata: null, primitivesValidated: 0, trianglesValidated: 0, binaryBytes: 0, errors: 0, warnings: 0, messages: [] };
  // Defer external-resource checks here; every emitted primitive and image is checked below.
  const metadata = await validator.validateString(sourceText, { uri: path.basename(filename), maxIssues: 100, ignoredIssues: ['IO_ERROR'] });
  report.metadata = metadata.issues;
  report.errors += metadata.issues.numErrors;
  report.warnings += metadata.issues.numWarnings;
  for (const issue of metadata.issues.messages) if (issue.severity <= 1) report.messages.push({ stage: 'metadata', ...issue });
  if (root.buffers.length !== 1) throw new Error('Chunk validation expects one external geometry buffer');
  const binaryPath = path.resolve(directory, decodeURIComponent(root.buffers[0].uri));
  const binaryLength = fs.statSync(binaryPath).size;
  if (binaryLength !== root.buffers[0].byteLength) throw new Error('Binary file size differs from declared byteLength');
  report.binaryBytes = binaryLength;
  const binary = fs.openSync(binaryPath, 'r');
  const imageCache = new Map();
  for (let meshIndex = 0; meshIndex < root.meshes.length; meshIndex++) {
    const mesh = root.meshes[meshIndex];
    for (let primitiveIndex = 0; primitiveIndex < mesh.primitives.length; primitiveIndex++) {
      const primitive = mesh.primitives[primitiveIndex];
      if (primitive.mode !== 4 || primitive.targets || primitive.extensions) throw new Error('Expected basic indexed triangle primitives');
      const accessorIds = [...new Set([...Object.values(primitive.attributes), primitive.indices])];
      const accessorMap = new Map(accessorIds.map((id, index) => [id, index]));
      const viewIds = [...new Set(accessorIds.map(id => root.accessors[id].bufferView))];
      const viewMap = new Map(viewIds.map((id, index) => [id, index]));
      const start = Math.min(...viewIds.map(id => root.bufferViews[id].byteOffset || 0));
      const end = Math.max(...viewIds.map(id => (root.bufferViews[id].byteOffset || 0) + root.bufferViews[id].byteLength));
      if (start < 0 || end > binaryLength || end < start) throw new Error('Primitive range outside binary file');
      const bytes = Buffer.alloc(end - start);
      for (let offset = 0; offset < bytes.length;) {
        const count = fs.readSync(binary, bytes, offset, bytes.length - offset, start + offset);
        if (!count) throw new Error('Truncated geometry buffer');
        offset += count;
      }
      const chunk = {
        ...root,
        buffers: [{ uri: '__geometry__', byteLength: bytes.length }],
        bufferViews: viewIds.map(id => ({ ...root.bufferViews[id], buffer: 0, byteOffset: (root.bufferViews[id].byteOffset || 0) - start })),
        accessors: accessorIds.map(id => ({ ...root.accessors[id], bufferView: viewMap.get(root.accessors[id].bufferView) })),
        meshes: [{ name: mesh.name, primitives: [{ ...primitive, indices: accessorMap.get(primitive.indices), attributes: Object.fromEntries(Object.entries(primitive.attributes).map(([key, id]) => [key, accessorMap.get(id)])) }] }],
        nodes: [{ mesh: 0 }], scenes: [{ nodes: [0] }], scene: 0,
      };
      const result = await validator.validateString(JSON.stringify(chunk), {
        uri: `${path.basename(filename)}#mesh${meshIndex}/primitive${primitiveIndex}`,
        maxIssues: 100,
        externalResourceFunction: async uri => {
          if (uri === '__geometry__') return bytes;
          if (!imageCache.has(uri)) imageCache.set(uri, fs.readFileSync(path.resolve(directory, decodeURIComponent(uri))));
          return imageCache.get(uri);
        },
      });
      report.errors += result.issues.numErrors;
      report.warnings += result.issues.numWarnings;
      for (const issue of result.issues.messages) if (issue.severity <= 1) report.messages.push({ mesh: meshIndex, primitive: primitiveIndex, ...issue });
      report.primitivesValidated++;
      report.trianglesValidated += root.accessors[primitive.indices].count / 3;
      if (report.primitivesValidated % 50 === 0) console.error(`Validated ${report.primitivesValidated} primitives (${report.trianglesValidated} triangles), RSS ${(process.memoryUsage().rss / 1048576).toFixed(1)} MiB`);
    }
  }
  fs.closeSync(binary);
  const text = JSON.stringify(report, null, 2) + '\n';
  if (process.argv[3]) fs.writeFileSync(process.argv[3], text, { flag: 'wx' });
  console.log(text);
  if (report.errors) process.exitCode = 1;
})().catch(error => { console.error(error); process.exitCode = 1; });
