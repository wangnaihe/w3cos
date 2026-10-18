# Shapes (hidden classes) + static inline caches (M1 item 10)

## Object layout

Before: an arena object was a `RefCell<JsObject>`. Its own properties sat in
a `PropertyMap`: `Option<Box<…>>` holding either 4 inline `(JsString, Value)`
pairs or a `HashMap` plus an order `Vec`. Keys were page-interned strings, so
every probe compared strings by resolving each key through the page arena
(a TLS lookup).

After: `PropertyMap` holds an `Rc<Shape>` plus a flat `Vec<Value>` of slots.

- **Shape tree.** Shapes form a per-thread transition tree keyed by property
  name. Objects that receive the same keys in the same order share one shape.
- **Lookup.** Shapes with up to 8 keys are scanned linearly; wider shapes
  build a key→slot index on first use.
- **Shape ids.** Every shape has a process-unique `ShapeId` (`ROOT_SHAPE_ID`
  = 1 is the empty shape on every thread).
- **Lifetime.** Shape keys are heap strings, so shapes survive
  `page_arena::reset`. Shapes never hold a `Value`, so refcounts, `WeakRef`
  and leak CI are unaffected.
- **Dictionary mode** (the old `HashMap` + order representation) is used:
  - after any successful `delete`;
  - past 64 own keys;
  - once the thread has created 32768 shapes.

  Dictionary objects never hit an inline cache.
- **Key order.** Own-key enumeration follows `OrdinaryOwnPropertyKeys`:
  canonical array-index keys first in ascending order, then insertion order.
  Before, everything was in insertion order. Object literals that the
  compiler builds from a `HashMap` still use sorted key order, as before.

### Why `set_property` took ~1300 ns

Each `try_set_property` call did three things:

1. Read `__w3cosRejectIndexedSet` and parsed the key as a `u32`, for every key.
2. Called `get_own_property_descriptor(key)` just to read `writable`. That
   built a 4-entry `HashMap<String, Value>` and a new arena object (interning
   4 keys). `writable` is always `true` for ordinary objects.
3. Ran `set_direct`, which page-interned the key and refreshed heap
   accounting, even on an overwrite.

Now:

- A descriptor is only built when a proxy `getOwnPropertyDescriptor` trap
  exists.
- The marker is only read for index keys.
- The setter lookup (`format!("__w3cos_setter_{key}")` plus a walk up the
  prototype chain) is skipped when no object on the chain has a `__w3cos_`
  marker key or a proxy. Each shape tracks this as a flag.
- Overwrites neither intern nor re-account.

## Static inline caches

For `GetProperty` / `SetProperty` with a proven constant key, `w3ir_aot`
emits:

```rust
{ static __W3COS_IC: w3cos_core::PropIc = w3cos_core::PropIc::new();
  obj.get_property_ic("key", &__W3COS_IC) }
```

`PropIc` is one `AtomicU64`, packed as `S:29 | PS:29 | slot:6`, accessed with
`Relaxed` ordering. It records one of two cases:

- **own**: receiver shape `S` → slot `i`;
- **proto** (get only): receiver shape `S` (which proves there is no own key
  and no own accessor) plus the shape `PS` of the direct prototype → slot `i`
  of the prototype.

Every hit re-checks the shapes, so there is no invalidation list:

- adding or deleting keys changes the shape (deletes also go to dictionary
  mode);
- `defineProperty` accessors, prototype swaps and prototype writes all change
  a checked shape and turn into a miss.

The miss path runs the generic code, then refills the cache. Shape ids are
process-unique, so a cache entry written by another thread can only miss.

### Not cached (generic path every time)

- Proxies, DOM nodes, arrays, functions and dictionary objects.
- Keys the generic path special-cases: `length`, array indices,
  `__proto__`, the DOM shape-bump keys and `__w3cos_` markers.
- Reads that produce `undefined`, which may need the accessor fallback.

`set` hits only overwrite an existing own data slot. Key adds still take the
generic path, which now costs about 30 ns.

`W3COS_AOT_NO_IC=1` turns IC emission off, for A/B measurement.

## Measurements

Box VM (8 vCPU), release builds.
- `arena_rc_bench` columns are the median of 3 interleaved runs.
- `leak_ci --iterations 200` (with the leak-detector feature) columns are
  the best of 2 interleaved rounds.
- *base* = aot-port c313d9f, *shapes* = this work with `W3COS_AOT_NO_IC=1`,
  *shapes+IC* = this work.

| arena_rc_bench (ns/op) | base | shapes | shapes+IC |
|---|---:|---:|---:|
| get_property (own) | 23.8 | 23.9 | **19.1** (IC) |
| set_property (overwrite) | 1403 | 35.1 | **15.2** (IC) |
| proto get | — | 32.4 | **21.4** (IC) |
| alloc+drop 2-key object | 852 | 306 | 306 |

| leak_ci scenario, 200 iterations (ms) | base | shapes | shapes+IC | IC hit rate |
|---|---:|---:|---:|---:|
| realistic_sample | 3474 | 1659 | **1571** | 100% (2.75M) |
| probe | 577 | 307 | **304** | 100% (600k) |
| classes | 587 | 411 | **246** | 100% (220k) |
| for_of | 226 | 210 | **142** | 100% (163k) |
| closures | 268 | 135 | **128** | 100% (60k) |
| collections_json_strings | 158 | 140 | **130** | 50% |

Hit rates cover the steady-state window only; warm-up misses (one per site
and shape) are excluded. With ICs on, every leak-CI scenario still returns
to its baseline exactly (zero residual).

On Apple M5, with part 1 only (shapes and fast set):
- get_property: 14.5 → 12.7 ns
- set_property: about 630 → 24 ns

## Not covered yet

- `Object.freeze` / `seal` / `preventExtensions`, `writable: false` and
  `configurable`. These were not modelled before (`Object.freeze` is a
  no-op) and are still not modelled. The natural next step is per-slot
  attribute bits in the shape.
- Add-property transition ICs, which would help constructors.
- Method-call ICs: `call_method_slice` has many receiver special cases.
- Prototype chains deeper than one level.
- Dynamic keys (`o[k]`).
- The IC hit path still borrows the page arena to resolve the slot pointer.
  Publishing the object chunk table, as the refcount fast path does for
  slot metadata, would remove that borrow.
