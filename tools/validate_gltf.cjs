// Optional development check: npm install --prefix /tmp/objtools-validator gltf-validator
// NODE_PATH=/tmp/objtools-validator/node_modules node tools/validate_gltf.cjs file.gltf ...
const fs = require('node:fs');
const path = require('node:path');
const validator = require('gltf-validator');
(async () => {
  if (process.argv.length < 3) throw new Error('Usage: node validate_gltf.cjs file.gltf ...');
  for (const filename of process.argv.slice(2)) {
    const report = await validator.validateBytes(new Uint8Array(fs.readFileSync(filename)), {
      uri: path.basename(filename),
      externalResourceFunction: async uri => new Uint8Array(fs.readFileSync(path.resolve(path.dirname(filename), decodeURIComponent(uri)))),
    });
    console.log(JSON.stringify({ file: filename, validator: validator.version(), issues: report.issues }, null, 2));
    if (report.issues.numErrors) process.exitCode = 1;
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
