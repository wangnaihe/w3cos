pub mod bigint;
pub mod binary;
mod builtins;
pub mod class;
pub mod collections;
pub mod heap;
pub mod js_string;
pub mod host;
pub mod host_modules;
pub mod intrinsics;
pub mod json;
pub mod module_registry;
mod object;
pub mod promise;
mod proxy;
mod reactive;
pub mod regexp;
mod value;
pub mod weak;
pub mod web;

pub use builtins::{
    Array, Error, ErrorValue, Map, Math, Object, RangeError, ResizeObserver, Set, array_value,
    console, dispatch_resize_observers, dispatch_resize_observers_bounded, document, error_class,
    error_instance, json_value, math_value, object_value, parseFloat, parseInt,
};
pub use object::JsObject;
pub use proxy::{ProxyBuilder, ProxyHandler, proxy_class};
pub use reactive::{Computed, Effect, Signal, batch, watch};
pub use js_string::JsString;
pub use value::{ArrayStorage, JsFunction, PanicValue, Value, throw_value, type_of};
