'use strict';
// `v8`: heap statistics from the host's memory figures. W3C OS apps are AOT
// compiled with reference counting, so there is no V8 heap or code space;
// the numbers report the process's resident memory.

const proc = require('process');
const { unsupported } = require('node:internal/w3cos_unsupported');

function memory() {
  if (typeof proc.memoryUsage === 'function') {
    try { return proc.memoryUsage(); } catch (error) { /* fall through */ }
  }
  return { rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 };
}

module.exports = {
  cachedDataVersionTag: function cachedDataVersionTag() { return 0; },
  getHeapStatistics: function getHeapStatistics() {
    const m = memory();
    return {
      total_heap_size: m.heapTotal, total_heap_size_executable: 0, total_physical_size: m.rss,
      total_available_size: 0, used_heap_size: m.heapUsed, heap_size_limit: 0,
      malloced_memory: 0, peak_malloced_memory: 0, does_zap_garbage: 0,
      number_of_native_contexts: 1, number_of_detached_contexts: 0,
      total_global_handles_size: 0, used_global_handles_size: 0, external_memory: m.external,
    };
  },
  getHeapSpaceStatistics: function getHeapSpaceStatistics() { return []; },
  getHeapCodeStatistics: function getHeapCodeStatistics() {
    return { code_and_metadata_size: 0, bytecode_and_metadata_size: 0, external_script_source_size: 0, cpu_profiler_metadata_size: 0 };
  },
  setFlagsFromString: function setFlagsFromString() {},
  setHeapSnapshotNearHeapLimit: function setHeapSnapshotNearHeapLimit() {},
  writeHeapSnapshot: function writeHeapSnapshot() { throw unsupported('v8.writeHeapSnapshot'); },
  getHeapSnapshot: function getHeapSnapshot() { throw unsupported('v8.getHeapSnapshot'); },
  serialize: function serialize() { throw unsupported('v8.serialize'); },
  deserialize: function deserialize() { throw unsupported('v8.deserialize'); },
  isStringOneByteRepresentation: function isStringOneByteRepresentation(s) {
    for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 255) return false;
    return true;
  },
  startupSnapshot: { isBuildingSnapshot: function isBuildingSnapshot() { return false; } },
};
