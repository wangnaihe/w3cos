'use strict';
// `@vscode/policy-watcher` (M3 item 20). On Linux the native module has no
// backend: `createWatcher` throws `TypeError: Unsupported platform`, and
// Code-OSS reads file-based policy instead (`FilePolicyService`). w3cos does
// the same on every platform for now: registry (Windows) and managed
// preferences (macOS) policy are not read yet.

function createWatcher() {
  throw new TypeError('Unsupported platform');
}

module.exports = { createWatcher };
