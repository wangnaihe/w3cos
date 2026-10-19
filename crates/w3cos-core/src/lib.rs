pub mod bigint;
pub mod binary;
mod builtins;
pub mod class;
pub mod collections;
pub mod heap;
pub mod host;
pub mod host_modules;
pub mod ic;
pub mod intrinsics;
pub mod js_string;
pub mod json;
#[cfg(feature = "leak-detector")]
pub mod leak_detector;
pub mod module_registry;
mod number_proto;
mod object;
pub mod page_arena;
pub mod promise;
mod property_map;
pub mod typed;
/// Hidden-class (shape) diagnostics.
pub mod shapes {
    pub use crate::property_map::{
        MAX_SHAPE_KEYS, MAX_SHAPES, ROOT_SHAPE_ID, ShapeId, shape_count,
    };
}
mod proxy;
mod reactive;
pub mod regexp;
mod value;
pub mod weak;
pub mod web;

pub use builtins::{
    Array, Error, ErrorValue, Map, Math, Object, RangeError, ResizeObserver, Set, array_value,
    console, dispatch_resize_observers, dispatch_resize_observers_bounded, document, error_class,
    error_instance, json_value, math_value, named_constructor_function, named_function,
    object_value, parseFloat, parseInt, process_value, reset_constructors, string_value,
};
pub use ic::{IcStats, PropIc};
pub use js_string::JsString;
pub use object::JsObject;
pub use proxy::{ProxyBuilder, ProxyHandler, proxy_class};
pub use reactive::{Computed, Effect, Signal, batch, watch};
pub use value::{
    AotCapture, AotCaptureMap, AotFactory, ArrayStorage, Completion, FunctionData, Immediate,
    JsArrayRef, JsFunction, JsObjectRef, PanicValue, Value, WeakJsObject, catch_js,
    catch_js_result, fast_collection_get, install_fast_collection_get, install_fast_dom_dispatch,
    install_length_write_hook, js_string_from_utf16_units, js_string_utf16_units,
    register_aot_function_meta, throw_value, type_of, unwrap_or_throw,
};
