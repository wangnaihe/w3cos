//! Static inline caches for constant-key property access (M1 item 10).
//!
//! AOT code emits one `static` [`PropIc`] per `obj.key` / `obj.key = v`
//! site whose key is a compile-time constant and calls
//! [`crate::Value::get_property_ic`] / [`crate::Value::set_property_ic`].
//! The cache remembers where the key lived the last time:
//!
//! - **own**: receiver shape `S` -> slot `i` of the receiver;
//! - **proto** (get only): receiver shape `S` (which proves the key is not
//!   an own property, nor an own accessor) + shape `PS` of the receiver's
//!   direct prototype -> slot `i` of that prototype.
//!
//! A hit re-validates the shapes on every access, so there is no explicit
//! invalidation: adding/deleting keys, defining accessors, switching to
//! dictionary mode, swapping the prototype or mutating the prototype all
//! change a checked shape (or leave the shape tree) and turn the next access
//! into a miss that takes the generic path and refills the cache.
//!
//! The generic path is always taken for keys the generic code special-cases
//! (`length`, array indices, DOM shape-bump keys, `__proto__`), for proxies,
//! DOM nodes, arrays, functions and dictionary-mode objects, and when a hit
//! would read `undefined` (getter fallback semantics).
//!
//! The cache is one `AtomicU64` (`Relaxed`; shapes ids are process-unique,
//! so a value written by another thread can only miss), packed as
//! `S:29 | PS:29 | slot:6`. Shape ids that do not fit in 29 bits are simply
//! not cached.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::property_map::{MAX_SHAPE_KEYS, ShapeId};

const SLOT_BITS: u32 = 6;
const SHAPE_BITS: u32 = 29;
const SHAPE_MASK: u64 = (1 << SHAPE_BITS) - 1;
const _: () = assert!(MAX_SHAPE_KEYS <= 1 << SLOT_BITS);

/// One property-access site's cache. `const`-constructible for `static`s.
pub struct PropIc(AtomicU64);

impl PropIc {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// `(receiver shape, prototype shape or 0 for own, slot)`.
    #[inline(always)]
    pub(crate) fn load(&self) -> Option<(ShapeId, ShapeId, usize)> {
        let word = self.0.load(Ordering::Relaxed);
        if word == 0 {
            return None;
        }
        let slot = (word & ((1 << SLOT_BITS) - 1)) as usize;
        let shape = ((word >> SLOT_BITS) & SHAPE_MASK) as ShapeId;
        let proto = (word >> (SLOT_BITS + SHAPE_BITS)) as ShapeId;
        Some((shape, proto, slot))
    }

    pub(crate) fn store(&self, shape: ShapeId, proto: ShapeId, slot: usize) {
        if shape as u64 > SHAPE_MASK || proto as u64 > SHAPE_MASK || slot >= 1 << SLOT_BITS {
            return;
        }
        let word = slot as u64
            | ((shape as u64) << SLOT_BITS)
            | ((proto as u64) << (SLOT_BITS + SHAPE_BITS));
        self.0.store(word, Ordering::Relaxed);
    }

    /// Forget the cached location (tests).
    pub fn clear(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

impl Default for PropIc {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-thread inline-cache counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IcStats {
    pub get_hits: u64,
    pub get_misses: u64,
    pub set_hits: u64,
    pub set_misses: u64,
}

impl IcStats {
    pub fn hits(&self) -> u64 {
        self.get_hits + self.set_hits
    }

    pub fn misses(&self) -> u64 {
        self.get_misses + self.set_misses
    }

    /// Hit rate in `[0, 1]` (`0` when no IC site ran).
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits() + self.misses();
        if total == 0 {
            0.0
        } else {
            self.hits() as f64 / total as f64
        }
    }
}

thread_local! {
    static STATS: [Cell<u64>; 4] = const {
        [Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0)]
    };
}

#[inline(always)]
pub(crate) fn count(which: usize) {
    STATS.with(|stats| stats[which].set(stats[which].get() + 1));
}

pub(crate) const GET_HIT: usize = 0;
pub(crate) const GET_MISS: usize = 1;
pub(crate) const SET_HIT: usize = 2;
pub(crate) const SET_MISS: usize = 3;

/// This thread's IC counters since the last [`reset_stats`].
pub fn stats() -> IcStats {
    STATS.with(|stats| IcStats {
        get_hits: stats[GET_HIT].get(),
        get_misses: stats[GET_MISS].get(),
        set_hits: stats[SET_HIT].get(),
        set_misses: stats[SET_MISS].get(),
    })
}

pub fn reset_stats() {
    STATS.with(|stats| stats.iter().for_each(|cell| cell.set(0)));
}

/// Keys whose generic get/set has side paths an IC must not skip.
#[inline(always)]
pub(crate) fn generic_only_key(key: &str) -> bool {
    matches!(
        key,
        "length"
            | "__proto__"
            | "id"
            | "className"
            | "textContent"
            | "appendChild"
            | "insertBefore"
            | "removeChild"
            | "replaceChild"
            | "createElement"
            | "createElementNS"
            | "createTextNode"
            | "push"
    ) || key.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || key.starts_with(crate::property_map::MARKER_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
    use std::collections::HashMap;

    fn obj(entries: &[(&str, f64)]) -> Value {
        let object = Value::object(HashMap::new());
        for (key, value) in entries {
            object.set_property(key, Value::Number(*value));
        }
        object
    }

    fn get(object: &Value, key: &str, ic: &PropIc) -> Value {
        let expected = object.get_property(key);
        let actual = object.get_property_ic(key, ic);
        assert!(
            actual.strict_eq(&expected),
            "IC get {key}: {actual:?} vs generic {expected:?}"
        );
        actual
    }

    fn delta(before: IcStats) -> IcStats {
        let now = stats();
        IcStats {
            get_hits: now.get_hits - before.get_hits,
            get_misses: now.get_misses - before.get_misses,
            set_hits: now.set_hits - before.set_hits,
            set_misses: now.set_misses - before.set_misses,
        }
    }

    #[test]
    fn encoding_round_trips_and_rejects_oversized() {
        let ic = PropIc::new();
        assert!(ic.load().is_none());
        ic.store(7, 0, 3);
        assert_eq!(ic.load(), Some((7, 0, 3)));
        ic.store((1 << SHAPE_BITS) - 1, (1 << SHAPE_BITS) - 1, 63);
        assert_eq!(
            ic.load(),
            Some(((1 << SHAPE_BITS) - 1, (1 << SHAPE_BITS) - 1, 63))
        );
        ic.clear();
        ic.store(1 << SHAPE_BITS, 0, 0);
        assert!(ic.load().is_none(), "ids past 29 bits are not cached");
    }

    #[test]
    fn own_get_hits_across_objects_with_one_shape() {
        let ic = PropIc::new();
        let objects: Vec<Value> = (0..10)
            .map(|i| obj(&[("x", i as f64), ("y", 1.0)]))
            .collect();
        let before = stats();
        for (i, object) in objects.iter().enumerate() {
            assert_eq!(get(object, "x", &ic).to_number(), i as f64);
        }
        let d = delta(before);
        assert_eq!(d.get_misses, 1);
        assert_eq!(d.get_hits, 9);
    }

    #[test]
    fn shape_change_misses_then_refills() {
        let ic = PropIc::new();
        let a = obj(&[("x", 1.0)]);
        let b = obj(&[("y", 0.0), ("x", 2.0)]); // x at a different slot
        get(&a, "x", &ic);
        let before = stats();
        assert_eq!(get(&b, "x", &ic).to_number(), 2.0);
        assert_eq!(get(&b, "x", &ic).to_number(), 2.0);
        let d = delta(before);
        assert_eq!((d.get_misses, d.get_hits), (1, 1));
        // Adding a key transitions `b` to a new shape: miss, correct value.
        b.set_property("z", Value::Number(9.0));
        assert_eq!(get(&b, "x", &ic).to_number(), 2.0);
    }

    #[test]
    fn delete_switches_to_dictionary_and_misses() {
        let ic = PropIc::new();
        let a = obj(&[("x", 1.0), ("y", 2.0)]);
        assert_eq!(get(&a, "y", &ic).to_number(), 2.0);
        a.delete_property("x");
        let before = stats();
        assert_eq!(get(&a, "y", &ic).to_number(), 2.0);
        assert_eq!(get(&a, "y", &ic).to_number(), 2.0);
        let d = delta(before);
        assert_eq!(d.get_hits, 0, "dictionary objects never hit");
        a.delete_property("y");
        assert!(get(&a, "y", &ic).is_undefined());
    }

    #[test]
    fn proto_lookup_hits_and_invalidates() {
        let ic = PropIc::new();
        let proto = obj(&[("method", 42.0)]);
        let make = || {
            let o = obj(&[("own", 1.0)]);
            crate::class::set_prototype_of(&o, &proto);
            o
        };
        let a = make();
        let b = make();
        get(&a, "method", &ic);
        let before = stats();
        assert_eq!(get(&b, "method", &ic).to_number(), 42.0);
        assert_eq!(delta(before).get_hits, 1, "proto IC hit");
        // Prototype value change: same shape, new value seen.
        proto.set_property("method", Value::Number(43.0));
        assert_eq!(get(&a, "method", &ic).to_number(), 43.0);
        // Prototype shape change: miss, still correct.
        proto.set_property("other", Value::Number(0.0));
        assert_eq!(get(&a, "method", &ic).to_number(), 43.0);
        // Own shadowing property: receiver shape changes.
        a.set_property("method", Value::Number(7.0));
        assert_eq!(get(&a, "method", &ic).to_number(), 7.0);
        assert_eq!(get(&b, "method", &ic).to_number(), 43.0);
        // Prototype swap to an object with the same shape but another value.
        let proto2 = obj(&[("method", 99.0), ("other", 0.0)]);
        crate::class::set_prototype_of(&b, &proto2);
        assert_eq!(get(&b, "method", &ic).to_number(), 99.0);
        // Prototype swap to null.
        crate::class::set_prototype_of(&b, &Value::Null);
        assert!(get(&b, "method", &ic).is_undefined());
    }

    #[test]
    fn accessors_take_the_generic_path() {
        let ic = PropIc::new();
        let a = obj(&[("x", 1.0)]);
        assert_eq!(get(&a, "x", &ic).to_number(), 1.0);
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = counter.clone();
        crate::class::define_property(
            &a,
            "x",
            &Value::object(HashMap::from([(
                "get".to_string(),
                Value::function(move |_, _| {
                    seen.set(seen.get() + 1);
                    Value::Number(5.0)
                }),
            )])),
        );
        assert_eq!(a.get_property_ic("x", &ic).to_number(), 5.0);
        assert_eq!(a.get_property_ic("x", &ic).to_number(), 5.0);
        assert_eq!(counter.get(), 2, "getter runs on every access");
        // Getter on the prototype + own data `undefined`: generic fallback.
        let proto = Value::object(HashMap::new());
        crate::class::define_property(
            &proto,
            "y",
            &Value::object(HashMap::from([(
                "get".to_string(),
                Value::function(|_, _| Value::Number(8.0)),
            )])),
        );
        let b = obj(&[]);
        crate::class::set_prototype_of(&b, &proto);
        assert_eq!(get(&b, "y", &ic).to_number(), 8.0);
        assert_eq!(get(&b, "y", &ic).to_number(), 8.0);
    }

    #[test]
    fn set_ic_overwrites_own_slots_and_respects_setters_and_proxies() {
        let ic = PropIc::new();
        let objects: Vec<Value> = (0..5).map(|_| obj(&[("x", 0.0), ("y", 0.0)])).collect();
        let before = stats();
        for (i, object) in objects.iter().enumerate() {
            object.set_property_ic("y", Value::Number(i as f64), &ic);
        }
        let d = delta(before);
        assert_eq!((d.set_misses, d.set_hits), (1, 4));
        for (i, object) in objects.iter().enumerate() {
            assert_eq!(object.get_property("y").to_number(), i as f64);
        }
        // Adding a new key goes through the generic path (setter check).
        let calls = std::rc::Rc::new(std::cell::Cell::new(0.0));
        let seen = calls.clone();
        let proto = Value::object(HashMap::new());
        crate::class::define_property(
            &proto,
            "y",
            &Value::object(HashMap::from([(
                "set".to_string(),
                Value::function(move |_, args| {
                    seen.set(args[0].to_number());
                    Value::Undefined
                }),
            )])),
        );
        let fresh = obj(&[("x", 0.0)]);
        crate::class::set_prototype_of(&fresh, &proto);
        fresh.set_property_ic("y", Value::Number(11.0), &ic);
        assert_eq!(calls.get(), 11.0, "inherited setter runs");
        assert!(!fresh.properties_has_own_for_test("y"));
        // Proxy set trap always runs.
        let trapped = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = trapped.clone();
        let handler = crate::ProxyBuilder::new()
            .set(move |_, _, _, _| {
                flag.set(true);
                true
            })
            .build();
        let proxied = Value::proxied_object(
            &[("x", Value::Number(0.0)), ("y", Value::Number(0.0))],
            handler,
        );
        proxied.set_property_ic("y", Value::Number(3.0), &ic);
        assert!(trapped.get());
    }

    #[test]
    fn proxy_get_trap_runs_even_with_matching_shape() {
        let ic = PropIc::new();
        let plain = obj(&[("x", 1.0)]);
        get(&plain, "x", &ic);
        let handler = crate::ProxyBuilder::new()
            .get(|_, _, _| Value::Number(77.0))
            .build();
        let proxied = Value::proxied_object(&[("x", Value::Number(1.0))], handler);
        assert_eq!(proxied.get_property_ic("x", &ic).to_number(), 77.0);
    }

    #[test]
    fn generic_only_keys_bypass_the_cache() {
        let ic = PropIc::new();
        let array = Value::array(vec![Value::Number(1.0), Value::Number(2.0)]);
        assert_eq!(array.get_property_ic("length", &ic).to_number(), 2.0);
        assert_eq!(array.get_property_ic("1", &ic).to_number(), 2.0);
        let object = obj(&[("length", 3.0)]);
        assert_eq!(object.get_property_ic("length", &ic).to_number(), 3.0);
        assert!(ic.load().is_none());
        assert!(Value::Undefined.get_property_ic("x", &ic).is_undefined());
        assert!(Value::Number(1.0).get_property_ic("x", &ic).is_undefined());
    }

    #[test]
    fn page_reset_does_not_confuse_the_cache() {
        let ic = PropIc::new();
        let a = obj(&[("x", 1.0)]);
        get(&a, "x", &ic);
        drop(a);
        crate::page_arena::reset();
        let b = obj(&[("x", 2.0)]);
        assert_eq!(get(&b, "x", &ic).to_number(), 2.0);
    }

    #[test]
    fn refcounts_stay_exact_through_ic_writes() {
        crate::page_arena::reset();
        let base = crate::page_arena::live_objects();
        let ic = PropIc::new();
        let holder = obj(&[("child", 0.0)]);
        for _ in 0..100 {
            let child = Value::object(HashMap::new());
            holder.set_property_ic("child", child, &ic);
        }
        holder.set_property_ic("child", Value::Null, &ic);
        assert_eq!(crate::page_arena::live_objects(), base + 1);
        drop(holder);
        assert_eq!(crate::page_arena::live_objects(), base);
    }
}
