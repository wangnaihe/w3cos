// Electron's process metric extensions (Code-OSS timerService reads
// `(await process.getProcessMemoryInfo()).residentSet`).
const keys = (value) => Object.keys(value).sort().join(',');
// The glue extends the shared host object once `process` is required.
console.log('shared', require('process') === process);
const info = process.getProcessMemoryInfo();
console.log('memory', info instanceof Promise);
console.log('system', keys(process.getSystemMemoryInfo()));
console.log('heap', typeof process.getHeapStatistics().usedHeapSize, typeof process.getHeapStatistics().heapSizeLimit);
console.log('blink', keys(process.getBlinkMemoryInfo()));
console.log('cpu', typeof process.getCPUUsage().percentCPUUsage, typeof process.getCPUUsage().idleWakeupsPerSecond);
console.log('io', typeof process.getIOCounters().readOperationCount);
console.log('created', typeof process.getCreationTime(), process.getCreationTime() <= Date.now());
info.then((memory) => {
  console.log('resident', keys(memory), typeof memory.residentSet);
});
