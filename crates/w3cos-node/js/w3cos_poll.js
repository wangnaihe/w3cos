'use strict';
// One poll loop for the handles backed by non-blocking host I/O (child
// processes, sockets). The host functions never block; this timer asks each
// open handle for progress and backs off while nothing happens. Only
// referenced handles keep the loop (and so the process) alive.

const handles = new Set();
let timer = null;
let idleTicks = 0;

function hasRef() {
  for (const handle of handles) if (handle._pollRef !== false) return true;
  return false;
}

function delay() {
  if (idleTicks < 50) return 1;
  if (idleTicks < 250) return 4;
  return 16;
}

function schedule() {
  if (timer !== null || !hasRef()) return;
  timer = setTimeout(tick, delay());
}

function tick() {
  timer = null;
  let progressed = false;
  for (const handle of Array.from(handles)) {
    if (handles.has(handle) && handle._poll()) progressed = true;
  }
  idleTicks = progressed ? 0 : idleTicks + 1;
  schedule();
}

exports.add = function add(handle) {
  handles.add(handle);
  idleTicks = 0;
  schedule();
};

exports.remove = function remove(handle) {
  handles.delete(handle);
};

// New work (a write, `ref()`): poll promptly again.
exports.wake = function wake() {
  if (timer !== null && idleTicks >= 50) {
    clearTimeout(timer);
    timer = null;
  }
  idleTicks = 0;
  schedule();
};
