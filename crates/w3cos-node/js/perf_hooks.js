'use strict';
// `perf_hooks`: the global `performance` plus observer and histogram shapes.
// Entries are not delivered to PerformanceObserver yet.

function Histogram() {
  this.min = 0; this.max = 0; this.mean = 0; this.stddev = 0; this.count = 0;
  this.exceeds = 0; this.percentiles = new Map();
}
Histogram.prototype.percentile = function percentile() { return 0; };
Histogram.prototype.reset = function reset() {};
Histogram.prototype.enable = function enable() { return true; };
Histogram.prototype.disable = function disable() { return true; };
Histogram.prototype.record = function record(value) {
  value = Number(value);
  this.count++;
  if (this.count === 1 || value < this.min) this.min = value;
  if (value > this.max) this.max = value;
  this.mean += (value - this.mean) / this.count;
};

class PerformanceObserver {
  constructor(callback) { this.callback = callback; }
  observe() {}
  disconnect() {}
  takeRecords() { return []; }
}
PerformanceObserver.supportedEntryTypes = ['mark', 'measure'];

module.exports = {
  performance: globalThis.performance,
  PerformanceObserver,
  PerformanceEntry: globalThis.PerformanceEntry,
  PerformanceMark: globalThis.PerformanceMark,
  PerformanceMeasure: globalThis.PerformanceMeasure,
  monitorEventLoopDelay: function monitorEventLoopDelay() { return new Histogram(); },
  createHistogram: function createHistogram() { return new Histogram(); },
  constants: {
    NODE_PERFORMANCE_GC_MAJOR: 4, NODE_PERFORMANCE_GC_MINOR: 1,
    NODE_PERFORMANCE_GC_INCREMENTAL: 8, NODE_PERFORMANCE_GC_WEAKCB: 16,
  },
};
