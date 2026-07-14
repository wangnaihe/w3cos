//! Android NDK integration (M1 skeleton).
//!
//! `W3cosActivity` in `templates/android/` loads `libw3cos_mobile.so` and calls
//! [`w3cos_mobile_run`](crate::w3cos_mobile_run) or JNI `nativeRun`.

use anyhow::Result;
use std::sync::OnceLock;
use w3cos_std::Component;

static APP_BUILDER: OnceLock<fn() -> Component> = OnceLock::new();

/// Register the app UI builder before the shell invokes the native entry.
pub fn set_app_builder(builder: fn() -> Component) {
    let _ = APP_BUILDER.set(builder);
}

pub fn run(builder: fn() -> Component) -> Result<()> {
    set_app_builder(builder);
    run_from_shell()
}

pub fn run_from_shell() -> Result<()> {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let builder = APP_BUILDER
        .get()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("mobile app builder not registered"))?;

    // M1: reuse desktop window path until NDK Surface + touch land.
    log::info!("w3cos mobile android backend (M1): using runtime window bridge");
    w3cos_runtime::run_app(builder)
}

/// JNI: `W3cosActivity.nativeRun(manifestPath)`
#[allow(non_snake_case)]
pub mod jni {
    use jni::JNIEnv;
    use jni::objects::{JClass, JString};
    use jni::sys::jint;

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_example_w3cos_W3cosActivity_nativeRun(
        mut env: JNIEnv,
        _class: JClass,
        _manifest_path: JString,
    ) -> jint {
        match crate::android::run_from_shell() {
            Ok(()) => 0,
            Err(e) => {
                let _ = env.throw_new(
                    "java/lang/RuntimeException",
                    format!("w3cos native run failed: {e:#}"),
                );
                1
            }
        }
    }
}
