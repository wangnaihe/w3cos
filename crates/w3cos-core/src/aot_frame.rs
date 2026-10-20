//! Out-of-line helpers for W3IR AOT frames.
//!
//! Generated frames used to inline the capture/binding protocol at every
//! binding access (capture getter, else TDZ-checked cell). On Code-OSS scale
//! bundles that boilerplate was ~45% of the emitted Rust; these helpers keep
//! the exact semantics behind one call each so the generated crates stay
//! compilable.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::{AotCapture, AotCaptureMap, Value, catch_js, intrinsics};

/// A W3IR binding cell: `(value, initialized)`.
pub type BindingCell = Rc<RefCell<(Value, bool)>>;

/// Slot containers used by generated frames: inline arrays and `Vec`s are
/// indexed by packed position, `HashMap`s by W3IR binding id.
pub trait Slots<T> {
    fn slot(&self, key: u32) -> Option<&T>;
    fn put(&mut self, key: u32, value: T);
}

impl<T, const N: usize> Slots<T> for [Option<T>; N] {
    #[inline]
    fn slot(&self, key: u32) -> Option<&T> {
        self.get(key as usize).and_then(|slot| slot.as_ref())
    }
    #[inline]
    fn put(&mut self, key: u32, value: T) {
        if let Some(slot) = self.get_mut(key as usize) {
            *slot = Some(value);
        }
    }
}

impl<T> Slots<T> for Vec<Option<T>> {
    #[inline]
    fn slot(&self, key: u32) -> Option<&T> {
        self.get(key as usize).and_then(|slot| slot.as_ref())
    }
    #[inline]
    fn put(&mut self, key: u32, value: T) {
        if let Some(slot) = self.get_mut(key as usize) {
            *slot = Some(value);
        }
    }
}

impl<T> Slots<T> for HashMap<u32, T> {
    #[inline]
    fn slot(&self, key: u32) -> Option<&T> {
        self.get(&key)
    }
    #[inline]
    fn put(&mut self, key: u32, value: T) {
        self.insert(key, value);
    }
}

#[cold]
#[inline(never)]
pub fn tdz_error() -> Value {
    intrinsics::reference_error("binding is not initialized")
}

#[inline(never)]
fn cell_value<B: Slots<BindingCell> + ?Sized>(bindings: &B, key: u32) -> Result<Value, Value> {
    match bindings.slot(key) {
        Some(cell) => {
            let binding = cell.borrow();
            if binding.1 {
                Ok(binding.0.clone())
            } else {
                Err(tdz_error())
            }
        }
        None => Err(tdz_error()),
    }
}

/// `LoadBinding`: a capture getter wins; otherwise the TDZ-checked cell.
/// The getter is called directly (a throw unwinds), as in sync frames
/// without an exception target.
#[inline(never)]
pub fn load<G, B>(getters: &G, bindings: &B, key: u32) -> Result<Value, Value>
where
    G: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    match getters.slot(key) {
        Some(getter) => Ok(getter.call_slice(Value::Undefined, &[])),
        None => cell_value(bindings, key),
    }
}

/// [`load`] with a getter throw converted to `Err`.
#[inline(never)]
pub fn load_catch<G, B>(getters: &G, bindings: &B, key: u32) -> Result<Value, Value>
where
    G: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    match getters.slot(key) {
        Some(getter) => catch_js(|| getter.call_slice(Value::Undefined, &[])),
        None => cell_value(bindings, key),
    }
}

/// The initialized cell for a fused in-place read (TDZ error otherwise).
#[inline(never)]
pub fn ready_cell<B: Slots<BindingCell> + ?Sized>(
    bindings: &B,
    key: u32,
) -> Result<&BindingCell, Value> {
    match bindings.slot(key) {
        Some(cell) if cell.borrow().1 => Ok(cell),
        _ => Err(tdz_error()),
    }
}

/// `InitializeBinding`.
#[inline(never)]
pub fn initialize<B: Slots<BindingCell> + ?Sized>(bindings: &mut B, key: u32, value: Value) {
    if let Some(binding) = bindings.slot(key) {
        *binding.borrow_mut() = (value, true);
    } else {
        bindings.put(key, Rc::new(RefCell::new((value, true))));
    }
}

#[inline(never)]
fn store_cell<B: Slots<BindingCell> + ?Sized>(
    bindings: &B,
    key: u32,
    value: Value,
) -> Result<(), Value> {
    match bindings.slot(key) {
        Some(binding) => {
            let mut binding = binding.borrow_mut();
            if !binding.1 {
                return Err(tdz_error());
            }
            binding.0 = value;
            Ok(())
        }
        None => Err(intrinsics::reference_error("missing binding")),
    }
}

/// `StoreBinding`: a capture setter wins (immutable captures throw a
/// TypeError); otherwise the TDZ-checked cell.
#[inline(never)]
pub fn store<S, B>(setters: &S, bindings: &B, key: u32, value: Value) -> Result<(), Value>
where
    S: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    match setters.slot(key) {
        Some(setter) => {
            if !setter.is_callable() {
                return Err(intrinsics::type_error("captured binding is immutable"));
            }
            setter.call_slice(Value::Undefined, std::slice::from_ref(&value));
            Ok(())
        }
        None => store_cell(bindings, key, value),
    }
}

/// [`store`] with a setter throw converted to `Err`.
#[inline(never)]
pub fn store_catch<S, B>(setters: &S, bindings: &B, key: u32, value: Value) -> Result<(), Value>
where
    S: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    match setters.slot(key) {
        Some(setter) => {
            if !setter.is_callable() {
                return Err(intrinsics::type_error("captured binding is immutable"));
            }
            catch_js(|| setter.call_slice(Value::Undefined, std::slice::from_ref(&value)))
                .map(|_| ())
        }
        None => store_cell(bindings, key, value),
    }
}

/// Fresh binding cells for a factory: `true` entries start initialized
/// (`var`/parameters), `false` ones in TDZ.
#[inline(never)]
pub fn fresh_cells<B: Slots<BindingCell> + ?Sized>(bindings: &mut B, keys: &[(u32, bool)]) {
    for &(key, initialized) in keys {
        bindings.put(key, Rc::new(RefCell::new((Value::Undefined, initialized))));
    }
}

/// Install a factory's capture environment. `ids` maps a W3IR binding id to
/// its packed slot position; an empty `ids` means slot key == binding id.
#[inline(never)]
pub fn bind_captures<G, S, B>(
    captures: &AotCaptureMap,
    ids: &[u32],
    getters: &mut G,
    setters: &mut S,
    bindings: &mut B,
) where
    G: Slots<Value> + ?Sized,
    S: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    for (binding, capture) in captures.iter() {
        let key = if ids.is_empty() {
            *binding
        } else {
            match ids.iter().position(|id| id == binding) {
                Some(index) => index as u32,
                None => continue,
            }
        };
        match capture {
            AotCapture::Accessors(getter, setter) => {
                getters.put(key, getter.clone());
                setters.put(key, setter.clone());
            }
            AotCapture::Cell(cell) => bindings.put(key, Rc::clone(cell)),
            AotCapture::Number(cell) => {
                let number = Rc::clone(cell);
                let number_set = Rc::clone(cell);
                getters.put(
                    key,
                    Value::function(move |_, _| match number.get() {
                        Some(n) => Value::Number(n),
                        None => Value::Undefined,
                    }),
                );
                setters.put(
                    key,
                    Value::function(move |_, arguments| {
                        let next = arguments
                            .first()
                            .map(|value| value.to_number())
                            .unwrap_or(f64::NAN);
                        number_set.set(Some(next));
                        Value::Undefined
                    }),
                );
            }
        }
    }
}

fn insert_capture_pair(
    nested: &mut AotCaptureMap,
    capture_id: u32,
    pair: (Value, Value),
    weaken: bool,
    inherited: bool,
) {
    if weaken {
        let target =
            catch_js(|| pair.0.call(Value::Undefined, Vec::new())).unwrap_or(Value::Undefined);
        if target.is_object() || target.is_array() || target.is_function() {
            let getter = crate::weak::weak_capture_getter(target);
            nested.insert(capture_id, AotCapture::Accessors(getter, Value::Undefined));
            return;
        }
    } else if inherited && let Some(target) = crate::weak::strong_capture_target(&pair.0) {
        nested.insert(
            capture_id,
            AotCapture::Accessors(crate::weak::strong_capture_getter(target), Value::Undefined),
        );
        return;
    }
    nested.insert(capture_id, AotCapture::Accessors(pair.0, pair.1));
}

/// `CreateClosure` capture adapter: forward one binding of this frame
/// (inherited capture accessors, or the local cell) into a nested
/// function's capture environment. Mirrors the inline adapter it replaces,
/// including weak back-edge handling.
#[inline(never)]
pub fn nest_capture<G, S, B>(
    nested: &mut AotCaptureMap,
    capture_id: u32,
    getters: &G,
    setters: &S,
    bindings: &B,
    key: u32,
    mutable: bool,
) where
    G: Slots<Value> + ?Sized,
    S: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    nest_capture_with(
        nested, capture_id, getters, setters, bindings, key, mutable, false,
    );
}

/// [`nest_capture`] for a unique back-edge: object targets are captured
/// through a WeakRef getter so the closure does not keep its owner alive.
#[inline(never)]
pub fn nest_capture_weak<G, S, B>(
    nested: &mut AotCaptureMap,
    capture_id: u32,
    getters: &G,
    setters: &S,
    bindings: &B,
    key: u32,
    mutable: bool,
) where
    G: Slots<Value> + ?Sized,
    S: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    nest_capture_with(
        nested, capture_id, getters, setters, bindings, key, mutable, true,
    );
}

#[allow(clippy::too_many_arguments)]
fn nest_capture_with<G, S, B>(
    nested: &mut AotCaptureMap,
    capture_id: u32,
    getters: &G,
    setters: &S,
    bindings: &B,
    key: u32,
    mutable: bool,
    weaken: bool,
) where
    G: Slots<Value> + ?Sized,
    S: Slots<Value> + ?Sized,
    B: Slots<BindingCell> + ?Sized,
{
    if let Some(getter) = getters.slot(key).cloned() {
        let setter = setters.slot(key).cloned().unwrap_or(Value::Undefined);
        insert_capture_pair(nested, capture_id, (getter, setter), weaken, true);
    } else if let Some(cell) = bindings.slot(key).cloned() {
        if mutable {
            if weaken {
                let target = cell.borrow().0.clone();
                if target.is_object() || target.is_array() || target.is_function() {
                    let getter = crate::weak::weak_capture_getter(target);
                    nested.insert(capture_id, AotCapture::Accessors(getter, Value::Undefined));
                    return;
                }
            }
            nested.insert(capture_id, AotCapture::Cell(cell));
        } else {
            let getter = Value::function(move |_, _| {
                let binding = cell.borrow();
                if binding.1 {
                    binding.0.clone()
                } else {
                    crate::throw_value(tdz_error())
                }
            });
            insert_capture_pair(
                nested,
                capture_id,
                (getter, Value::Undefined),
                weaken,
                false,
            );
        }
    } else {
        crate::throw_value(intrinsics::reference_error(
            "missing nested generator capture",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_store_follow_capture_then_cell() {
        let mut bindings: [Option<BindingCell>; 2] = std::array::from_fn(|_| None);
        let getters: [Option<Value>; 2] = std::array::from_fn(|_| None);
        let setters: [Option<Value>; 2] = std::array::from_fn(|_| None);
        fresh_cells(&mut bindings, &[(0, true), (1, false)]);
        assert_eq!(
            load(&getters, &bindings, 0).unwrap().to_js_string(),
            "undefined"
        );
        assert!(load(&getters, &bindings, 1).is_err());
        assert!(store(&setters, &bindings, 1, Value::Number(1.0)).is_err());
        initialize(&mut bindings, 1, Value::Number(2.0));
        assert!(store(&setters, &bindings, 1, Value::Number(3.0)).is_ok());
        assert_eq!(load(&getters, &bindings, 1).unwrap().to_number(), 3.0);
        assert!(ready_cell(&bindings, 1).is_ok());
    }

    #[test]
    fn bind_captures_maps_packed_ids() {
        let mut bindings: Vec<Option<BindingCell>> = vec![None; 2];
        let mut getters: Vec<Option<Value>> = vec![None; 2];
        let mut setters: Vec<Option<Value>> = vec![None; 2];
        let mut captures = AotCaptureMap::new();
        captures.insert(
            40,
            AotCapture::Cell(Rc::new(RefCell::new((Value::Number(7.0), true)))),
        );
        captures.insert(
            99,
            AotCapture::Number(Rc::new(std::cell::Cell::new(Some(1.0)))),
        );
        bind_captures(
            &captures,
            &[39, 40],
            &mut getters,
            &mut setters,
            &mut bindings,
        );
        assert_eq!(load(&getters, &bindings, 1).unwrap().to_number(), 7.0);
        assert!(getters.iter().all(Option::is_none));
    }
}
