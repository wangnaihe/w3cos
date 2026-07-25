//! WebXR capability, geometry and interface compatibility.
//!
//! XR hardware sessions require a compositor/device adapter. Capability checks
//! therefore return `false` and session requests reject, while device-independent
//! XRRigidTransform and XRRay math is fully available.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static XR_SYSTEM: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn number(value: Value, fallback: f64) -> f64 {
    if value.is_undefined() {
        fallback
    } else {
        value.to_number()
    }
}

fn error(name: &str, message: &str) -> Value {
    if name == "TypeError" {
        w3cos_core::error_instance(name, vec![Value::string(message)])
    } else {
        w3cos_core::web::dom_exception_instance(message, name)
    }
}

fn throw(name: &str, message: &str) -> ! {
    w3cos_core::throw_value(error(name, message))
}

fn warn_once() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: WebXR geometry and capability surfaces are available; immersive \
                 sessions, tracking, camera/depth data and XR compositor layers require a native \
                 XR device adapter"
            );
        }
    });
}

fn unavailable(api: &str) -> Value {
    warn_once();
    w3cos_core::promise::reject(vec![error(
        "NotSupportedError",
        &format!("{api} requires a native XR device and compositor adapter"),
    )])
}

fn point(values: [f64; 4]) -> Value {
    w3cos_core::class::construct(
        &crate::geometry_web::class("DOMPointReadOnly"),
        values.into_iter().map(Value::Number).collect(),
    )
}

fn matrix(values: [f64; 16]) -> Value {
    w3cos_core::class::construct(
        &crate::geometry_web::class("DOMMatrixReadOnly"),
        vec![Value::array(
            values.into_iter().map(Value::Number).collect(),
        )],
    )
}

fn point_init(init: Value, defaults: [f64; 4]) -> [f64; 4] {
    [
        number(init.get_property("x"), defaults[0]),
        number(init.get_property("y"), defaults[1]),
        number(init.get_property("z"), defaults[2]),
        number(init.get_property("w"), defaults[3]),
    ]
}

fn normalized_quaternion(mut q: [f64; 4]) -> [f64; 4] {
    let length = q
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt();
    if !length.is_finite() || length <= f64::EPSILON {
        throw(
            "TypeError",
            "XRRigidTransform orientation must be a non-zero quaternion",
        );
    }
    for component in &mut q {
        *component /= length;
    }
    q
}

fn transform_matrix(position: [f64; 4], q: [f64; 4]) -> [f64; 16] {
    let [x, y, z, w] = q;
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        0.0,
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        0.0,
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
        0.0,
        position[0],
        position[1],
        position[2],
        1.0,
    ]
}

fn inverse_components(position: [f64; 4], q: [f64; 4]) -> ([f64; 4], [f64; 4]) {
    let inverse_q = [-q[0], -q[1], -q[2], q[3]];
    let matrix = transform_matrix([0.0, 0.0, 0.0, 1.0], inverse_q);
    let x = -(matrix[0] * position[0] + matrix[4] * position[1] + matrix[8] * position[2]);
    let y = -(matrix[1] * position[0] + matrix[5] * position[1] + matrix[9] * position[2]);
    let z = -(matrix[2] * position[0] + matrix[6] * position[1] + matrix[10] * position[2]);
    ([x, y, z, 1.0], inverse_q)
}

fn rigid_transform_value(position: [f64; 4], orientation: [f64; 4]) -> Value {
    let value = Value::object(HashMap::from([
        ("position".into(), point(position)),
        ("orientation".into(), point(orientation)),
        (
            "matrix".into(),
            matrix(transform_matrix(position, orientation)),
        ),
    ]));
    value.set_property(
        "__w3cos_getter_inverse",
        Value::function(move |_, _| {
            let (inverse_position, inverse_orientation) = inverse_components(position, orientation);
            rigid_transform_value(inverse_position, inverse_orientation)
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("XRRigidTransform").get_property("prototype"),
    );
    value
}

fn construct_rigid_transform(args: Vec<Value>) -> Value {
    let position = point_init(arg(&args, 0), [0.0, 0.0, 0.0, 1.0]);
    if (position[3] - 1.0).abs() > f64::EPSILON {
        throw("TypeError", "XRRigidTransform position.w must be 1");
    }
    let orientation = normalized_quaternion(point_init(arg(&args, 1), [0.0, 0.0, 0.0, 1.0]));
    rigid_transform_value(position, orientation)
}

fn ray_matrix(origin: [f64; 4], direction: [f64; 4]) -> [f64; 16] {
    let forward = [-direction[0], -direction[1], -direction[2]];
    let up_seed = if forward[1].abs() > 0.999 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let mut right = [
        up_seed[1] * forward[2] - up_seed[2] * forward[1],
        up_seed[2] * forward[0] - up_seed[0] * forward[2],
        up_seed[0] * forward[1] - up_seed[1] * forward[0],
    ];
    let right_length = right.iter().map(|x| x * x).sum::<f64>().sqrt();
    for value in &mut right {
        *value /= right_length;
    }
    let up = [
        forward[1] * right[2] - forward[2] * right[1],
        forward[2] * right[0] - forward[0] * right[2],
        forward[0] * right[1] - forward[1] * right[0],
    ];
    [
        right[0], right[1], right[2], 0.0, up[0], up[1], up[2], 0.0, forward[0], forward[1],
        forward[2], 0.0, origin[0], origin[1], origin[2], 1.0,
    ]
}

fn construct_ray(args: Vec<Value>) -> Value {
    let origin = point_init(arg(&args, 0), [0.0, 0.0, 0.0, 1.0]);
    if (origin[3] - 1.0).abs() > f64::EPSILON {
        throw("TypeError", "XRRay origin.w must be 1");
    }
    let mut direction = point_init(arg(&args, 1), [0.0, 0.0, -1.0, 0.0]);
    if direction[3].abs() > f64::EPSILON {
        throw("TypeError", "XRRay direction.w must be 0");
    }
    let length = direction[..3]
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt();
    if !length.is_finite() || length <= f64::EPSILON {
        throw("TypeError", "XRRay direction must be non-zero");
    }
    for component in &mut direction[..3] {
        *component /= length;
    }
    let value = Value::object(HashMap::from([
        ("origin".into(), point(origin)),
        ("direction".into(), point(direction)),
        ("matrix".into(), matrix(ray_matrix(origin, direction))),
    ]));
    w3cos_core::class::set_prototype_of(&value, &class_for("XRRay").get_property("prototype"));
    value
}

fn members(name: &str) -> &'static [&'static str] {
    match name {
        "XRAnchor" => &["anchorSpace", "delete"],
        "XRAnchorSet" | "XRPlaneSet" => &["size", "entries", "forEach", "has", "keys", "values"],
        "XRBoundedReferenceSpace" => &["boundsGeometry"],
        "XRCPUDepthInformation" => &["data", "getDepthInMeters"],
        "XRCamera" => &["width", "height"],
        "XRCompositionLayer" => &[
            "layout",
            "blendTextureSourceAlpha",
            "forceMonoPresentation",
            "opacity",
            "mipLevels",
            "needsRedraw",
            "destroy",
        ],
        "XRCubeLayer" => &["space", "orientation", "onredraw"],
        "XRCylinderLayer" => &[
            "space",
            "transform",
            "radius",
            "centralAngle",
            "aspectRatio",
            "onredraw",
        ],
        "XRDOMOverlayState" => &["type"],
        "XRDepthInformation" => &[
            "width",
            "height",
            "normDepthBufferFromNormView",
            "rawValueToMeters",
            "projectionMatrix",
            "transform",
        ],
        "XREquirectLayer" => &[
            "space",
            "transform",
            "radius",
            "centralHorizontalAngle",
            "upperVerticalAngle",
            "lowerVerticalAngle",
            "onredraw",
        ],
        "XRFrame" => &[
            "session",
            "getPose",
            "getViewerPose",
            "trackedAnchors",
            "createAnchor",
            "fillJointRadii",
            "fillPoses",
            "getDepthInformation",
            "getHitTestResults",
            "getHitTestResultsForTransientInput",
            "getJointPose",
            "getLightEstimate",
            "detectedPlanes",
        ],
        "XRHand" => &["size", "get", "entries", "forEach", "keys", "values"],
        "XRHitTestResult" => &["getPose", "createAnchor"],
        "XRHitTestSource" | "XRTransientInputHitTestSource" => &["cancel"],
        "XRInputSource" => &[
            "handedness",
            "targetRayMode",
            "targetRaySpace",
            "gripSpace",
            "gamepad",
            "hand",
            "profiles",
        ],
        "XRInputSourceArray" => &["entries", "keys", "values", "forEach", "length"],
        "XRJointPose" => &["radius"],
        "XRJointSpace" => &["jointName"],
        "XRLayer" | "XRSpace" => &[],
        "XRLightEstimate" => &[
            "sphericalHarmonicsCoefficients",
            "primaryLightDirection",
            "primaryLightIntensity",
        ],
        "XRLightProbe" => &["probeSpace", "onreflectionchange"],
        "XRPlane" => &[
            "planeSpace",
            "polygon",
            "orientation",
            "lastChangedTime",
            "semanticLabel",
        ],
        "XRPose" => &["transform", "emulatedPosition"],
        "XRProjectionLayer" => &[
            "textureWidth",
            "textureHeight",
            "textureArrayLength",
            "ignoreDepthValues",
            "fixedFoveation",
            "deltaPose",
        ],
        "XRQuadLayer" => &["space", "transform", "width", "height", "onredraw"],
        "XRRay" => &["origin", "direction", "matrix"],
        "XRReferenceSpace" => &["onreset", "getOffsetReferenceSpace"],
        "XRRenderState" => &[
            "depthNear",
            "depthFar",
            "inlineVerticalFieldOfView",
            "baseLayer",
            "layers",
        ],
        "XRRigidTransform" => &["position", "orientation", "matrix", "inverse"],
        "XRSession" => &[
            "environmentBlendMode",
            "interactionMode",
            "visibilityState",
            "renderState",
            "inputSources",
            "domOverlayState",
            "preferredReflectionFormat",
            "onend",
            "onselect",
            "oninputsourceschange",
            "onselectstart",
            "onselectend",
            "onvisibilitychange",
            "onsqueeze",
            "onsqueezestart",
            "onsqueezeend",
            "depthUsage",
            "depthDataFormat",
            "depthType",
            "depthActive",
            "cancelAnimationFrame",
            "end",
            "pauseDepthSensing",
            "requestAnimationFrame",
            "requestHitTestSource",
            "requestHitTestSourceForTransientInput",
            "requestLightProbe",
            "requestReferenceSpace",
            "resumeDepthSensing",
            "updateRenderState",
            "enabledFeatures",
            "maxRenderLayers",
            "onvisibilitymaskchange",
            "initiateRoomCapture",
        ],
        "XRSubImage" => &["viewport"],
        "XRSystem" => &["ondevicechange", "isSessionSupported", "requestSession"],
        "XRTransientInputHitTestResult" => &["inputSource", "results"],
        "XRView" => &[
            "eye",
            "recommendedViewportScale",
            "isFirstPersonObserver",
            "camera",
            "requestViewportScale",
            "index",
            "projectionMatrix",
            "transform",
        ],
        "XRViewerPose" => &["views"],
        "XRViewport" => &["x", "y", "width", "height"],
        "XRWebGLBinding" => &[
            "nativeProjectionScaleFactor",
            "usesDepthValues",
            "createCubeLayer",
            "createCylinderLayer",
            "createEquirectLayer",
            "createProjectionLayer",
            "createQuadLayer",
            "getSubImage",
            "getViewSubImage",
            "getCameraImage",
            "getDepthInformation",
            "getReflectionCubeMap",
        ],
        "XRWebGLDepthInformation" => &["texture"],
        "XRWebGLLayer" => &[
            "antialias",
            "ignoreDepthValues",
            "framebufferWidth",
            "framebufferHeight",
            "framebuffer",
            "getViewport",
        ],
        "XRWebGLSubImage" => &[
            "colorTexture",
            "depthStencilTexture",
            "motionVectorTexture",
            "imageIndex",
            "colorTextureWidth",
            "colorTextureHeight",
            "depthStencilTextureWidth",
            "depthStencilTextureHeight",
            "motionVectorTextureWidth",
            "motionVectorTextureHeight",
        ],
        _ => &[],
    }
}

fn parent(name: &str) -> Option<&'static str> {
    match name {
        "XRBoundedReferenceSpace" => Some("XRReferenceSpace"),
        "XRReferenceSpace" | "XRJointSpace" => Some("XRSpace"),
        "XRCPUDepthInformation" | "XRWebGLDepthInformation" => Some("XRDepthInformation"),
        "XRCubeLayer" | "XRCylinderLayer" | "XREquirectLayer" | "XRProjectionLayer"
        | "XRQuadLayer" => Some("XRCompositionLayer"),
        "XRCompositionLayer" => Some("XRLayer"),
        "XRJointPose" | "XRViewerPose" => Some("XRPose"),
        _ => None,
    }
}

fn event_target(name: &str) -> bool {
    matches!(
        name,
        "XRAnchor"
            | "XRLayer"
            | "XRLightProbe"
            | "XRReferenceSpace"
            | "XRSession"
            | "XRSpace"
            | "XRSystem"
    )
}

fn build_class(name: &'static str) -> Value {
    let constructor = match name {
        "XRRigidTransform" => Value::function(|_, args| construct_rigid_transform(args)),
        "XRRay" => Value::function(|_, args| construct_ray(args)),
        _ => {
            Value::function(move |_, _| throw("TypeError", &format!("Illegal constructor: {name}")))
        }
    };
    constructor.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
    for member in members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if let Some(parent) = parent(name) {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &class_for(parent).get_property("prototype"),
        );
    } else if event_target(name) {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
    }
    if name == "XRWebGLLayer" {
        constructor.set_property(
            "getNativeFramebufferScaleFactor",
            Value::function(|_, _| {
                warn_once();
                Value::Number(1.0)
            }),
        );
    }
    constructor.set_property("prototype", prototype);
    constructor
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn xr_system_value() -> Value {
    XR_SYSTEM.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([
            ("ondevicechange".into(), Value::Null),
            (
                "isSessionSupported".into(),
                Value::function(|_, args| {
                    let mode = arg(&args, 0).to_js_string();
                    if !matches!(mode.as_str(), "inline" | "immersive-vr" | "immersive-ar") {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "XR session mode must be inline, immersive-vr or immersive-ar",
                        )]);
                    }
                    warn_once();
                    w3cos_core::promise::resolve(vec![Value::Bool(false)])
                }),
            ),
            (
                "requestSession".into(),
                Value::function(|_, args| {
                    if args.is_empty() {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "XRSystem.requestSession requires a session mode",
                        )]);
                    }
                    unavailable("XRSystem.requestSession")
                }),
            ),
        ]));
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        w3cos_core::class::set_prototype_of(
            &value,
            &class_for("XRSystem").get_property("prototype"),
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub const INTERFACES: &[&str] = &[
    "XRAnchor",
    "XRAnchorSet",
    "XRBoundedReferenceSpace",
    "XRCPUDepthInformation",
    "XRCamera",
    "XRCompositionLayer",
    "XRCubeLayer",
    "XRCylinderLayer",
    "XRDOMOverlayState",
    "XRDepthInformation",
    "XREquirectLayer",
    "XRFrame",
    "XRHand",
    "XRHitTestResult",
    "XRHitTestSource",
    "XRInputSource",
    "XRInputSourceArray",
    "XRJointPose",
    "XRJointSpace",
    "XRLayer",
    "XRLightEstimate",
    "XRLightProbe",
    "XRPlane",
    "XRPlaneSet",
    "XRPose",
    "XRProjectionLayer",
    "XRQuadLayer",
    "XRRay",
    "XRReferenceSpace",
    "XRRenderState",
    "XRRigidTransform",
    "XRSession",
    "XRSpace",
    "XRSubImage",
    "XRSystem",
    "XRTransientInputHitTestResult",
    "XRTransientInputHitTestSource",
    "XRView",
    "XRViewerPose",
    "XRViewport",
    "XRWebGLBinding",
    "XRWebGLDepthInformation",
    "XRWebGLLayer",
    "XRWebGLSubImage",
];

pub fn reset() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    XR_SYSTEM.with(|slot| *slot.borrow_mut() = None);
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rigid_transform_normalizes_orientation_and_inverts_translation() {
        reset();
        let transform = w3cos_core::class::construct(
            &class_for("XRRigidTransform"),
            vec![
                Value::object(HashMap::from([
                    ("x".into(), Value::Number(2.0)),
                    ("y".into(), Value::Number(3.0)),
                    ("z".into(), Value::Number(4.0)),
                ])),
                Value::object(HashMap::from([("w".into(), Value::Number(2.0))])),
            ],
        );
        assert_eq!(
            transform
                .get_property("orientation")
                .get_property("w")
                .to_number(),
            1.0
        );
        let inverse = transform.get_property("inverse");
        assert_eq!(
            inverse
                .get_property("position")
                .get_property("x")
                .to_number(),
            -2.0
        );
    }

    #[test]
    fn ray_normalizes_direction_and_provides_matrix() {
        reset();
        let ray = w3cos_core::class::construct(
            &class_for("XRRay"),
            vec![
                Value::Undefined,
                Value::object(HashMap::from([("z".into(), Value::Number(-4.0))])),
            ],
        );
        assert_eq!(
            ray.get_property("direction").get_property("z").to_number(),
            -1.0
        );
        assert!(w3cos_core::class::instance_of(
            &ray.get_property("matrix"),
            &crate::geometry_web::class("DOMMatrixReadOnly")
        ));
    }

    #[test]
    fn xr_system_never_claims_missing_hardware() {
        reset();
        let system = xr_system_value();
        assert!(w3cos_core::class::instance_of(
            &system,
            &class_for("XRSystem")
        ));
        assert!(
            system
                .call_method("isSessionSupported", vec![Value::string("immersive-vr")])
                .is_object()
        );
        assert!(
            system
                .call_method("requestSession", vec![Value::string("immersive-vr")])
                .is_object()
        );
    }
}
