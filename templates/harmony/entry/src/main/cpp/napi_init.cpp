#include <ace/xcomponent/native_interface_xcomponent.h>
#include <hilog/log.h>
#include <napi/native_api.h>

namespace {
constexpr unsigned int LOG_DOMAIN = 0x0000;
constexpr const char *LOG_TAG = "w3cos";

void OnSurfaceCreated(OH_NativeXComponent *, void *window) {
  OH_LOG_Print(LOG_APP, LOG_INFO, LOG_DOMAIN, LOG_TAG,
               "XComponent surface created: %{public}p", window);
}

void OnSurfaceChanged(OH_NativeXComponent *, void *window) {
  OH_LOG_Print(LOG_APP, LOG_INFO, LOG_DOMAIN, LOG_TAG,
               "XComponent surface changed: %{public}p", window);
}

void OnSurfaceDestroyed(OH_NativeXComponent *, void *) {
  OH_LOG_Print(LOG_APP, LOG_INFO, LOG_DOMAIN, LOG_TAG,
               "XComponent surface destroyed");
}

OH_NativeXComponent_Callback CALLBACK = {
    .OnSurfaceCreated = OnSurfaceCreated,
    .OnSurfaceChanged = OnSurfaceChanged,
    .OnSurfaceDestroyed = OnSurfaceDestroyed,
    .DispatchTouchEvent = nullptr,
};

napi_value Init(napi_env env, napi_value exports) {
  napi_value exportInstance = nullptr;
  OH_NativeXComponent *component = nullptr;
  if (napi_get_named_property(env, exports, OH_NATIVE_XCOMPONENT_OBJ,
                              &exportInstance) != napi_ok ||
      napi_unwrap(env, exportInstance,
                  reinterpret_cast<void **>(&component)) != napi_ok ||
      component == nullptr) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, LOG_DOMAIN, LOG_TAG,
                 "Unable to resolve OH_NativeXComponent");
    return exports;
  }
  OH_NativeXComponent_RegisterCallback(component, &CALLBACK);
  return exports;
}
} // namespace

static napi_module MODULE = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "w3cos_harmony_host",
    .nm_priv = nullptr,
    .reserved = {0},
};

extern "C" __attribute__((constructor)) void RegisterW3cosHarmonyHost() {
  napi_module_register(&MODULE);
}
