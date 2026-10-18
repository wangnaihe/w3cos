// Numeric microbenchmarks for typed W3IR / dual-version functions
// (M1 item 11). Run with the leak CI harness, which also enforces zero
// residual:
//   cargo run --release -p w3cos-compiler --example leak_ci -- \
//       --scenarios crates/w3cos-compiler/bench-scenarios/numeric.ts --iterations 20
// `W3COS_AOT_NO_TYPED=1` at compile time turns typed versions off (A/B).

function sumLoop(n: number, step: number): number {
  let s = 0;
  for (let i = 0; i < n; i++) {
    s += i * step;
  }
  return s;
}
export function scenario_loop(): number {
  return sumLoop(200000, 0.5);
}

function mandel(w: number, h: number, iters: number): number {
  let inside = 0;
  for (let py = 0; py < h; py++) {
    for (let px = 0; px < w; px++) {
      const cx = (px / w) * 3 - 2;
      const cy = (py / h) * 2 - 1;
      let x = 0;
      let y = 0;
      let k = 0;
      while (k < iters && x * x + y * y <= 4) {
        const t = x * x - y * y + cx;
        y = 2 * x * y + cy;
        x = t;
        k++;
      }
      if (k === iters) inside++;
    }
  }
  return inside;
}
export function scenario_mandel(): number {
  return mandel(64, 48, 50);
}

function fib(n: number): number {
  return n < 2 ? n : fib(n - 1) + fib(n - 2);
}
export function scenario_fib(): number {
  return fib(18);
}

function clampMix(a: number, b: number, t: number): number {
  const u = t < 0 ? 0 : t > 1 ? 1 : t;
  return a + (b - a) * u;
}
export function scenario_calls(): number {
  let s = 0;
  for (let i = 0; i < 20000; i++) {
    s += clampMix(i, i * 2, (i % 7) / 5);
  }
  return s;
}

interface Body {
  x: number;
  y: number;
  z: number;
  vx: number;
  vy: number;
  vz: number;
  mass: number;
}
function makeBodies(): Body[] {
  const out: Body[] = [];
  for (let i = 0; i < 5; i++) {
    out.push({ x: i, y: i * 0.5, z: -i, vx: 0.01 * i, vy: 0, vz: 0.02, mass: 1 + i });
  }
  return out;
}
function advance(bodies: Body[], dt: number, n: number): void {
  for (let i = 0; i < n; i++) {
    const bi = bodies[i];
    for (let j = i + 1; j < n; j++) {
      const bj = bodies[j];
      const dx = bi.x - bj.x;
      const dy = bi.y - bj.y;
      const dz = bi.z - bj.z;
      const d2 = dx * dx + dy * dy + dz * dz + 0.01;
      const mag = dt / (d2 * Math.sqrt(d2));
      bi.vx -= dx * bj.mass * mag;
      bi.vy -= dy * bj.mass * mag;
      bi.vz -= dz * bj.mass * mag;
      bj.vx += dx * bi.mass * mag;
      bj.vy += dy * bi.mass * mag;
      bj.vz += dz * bi.mass * mag;
    }
  }
  for (let i = 0; i < n; i++) {
    const b = bodies[i];
    b.x += dt * b.vx;
    b.y += dt * b.vy;
    b.z += dt * b.vz;
  }
}
export function scenario_nbody(): number {
  const bodies = makeBodies();
  for (let s = 0; s < 200; s++) advance(bodies, 0.01, 5);
  return bodies[0].x;
}

function fannkuch(n: number): number {
  const perm1: number[] = [];
  for (let i = 0; i < n; i++) perm1.push(i);
  const perm: number[] = [];
  for (let i = 0; i < n; i++) perm.push(0);
  let maxFlips = 0;
  let checks = 0;
  // Rotate through n * (n - 1) starting permutations (bounded workload).
  for (let r = 0; r < n * (n - 1); r++) {
    for (let i = 0; i < n; i++) perm[i] = perm1[(i + r) % n];
    let flips = 0;
    let k = perm[0];
    while (k !== 0 && flips < 64) {
      let lo = 0;
      let hi = k;
      while (lo < hi) {
        const t = perm[lo];
        perm[lo] = perm[hi];
        perm[hi] = t;
        lo++;
        hi--;
      }
      flips++;
      k = perm[0];
    }
    if (flips > maxFlips) maxFlips = flips;
    checks += flips;
  }
  return maxFlips * 1000 + checks;
}
export function scenario_fannkuch(): number {
  return fannkuch(9);
}

// Mixed arguments: the same typed function called with numbers and with
// strings / missing arguments must take the generic version (guard failure)
// and keep JS semantics.
function scale(v: number, k: number): any {
  return v * k;
}
export function scenario_guard_failures(): number {
  let s = 0;
  for (let i = 0; i < 1000; i++) {
    s += scale(i, 2);
    if (i % 10 === 0) s += scale("3" as any, 2);
    if (i % 50 === 0) {
      const r = scale(i as any, undefined as any);
      s += r !== r ? 1 : 0;
    }
  }
  return s;
}
