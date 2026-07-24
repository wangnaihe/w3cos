#include <ace/xcomponent/native_interface_xcomponent.h>
#include <dlfcn.h>
#include <hilog/log.h>
#include <napi/native_api.h>

namespace {
constexpr unsigned int LOG_DOMAIN = 0x0000;
constexpr const char *LOG_TAG = "w3cos";

using SurfaceCreated = int32_t (*)(void *, uint32_t, uint32_t);
using SurfaceChanged = int32_t (*)(uint32_t, uint32_t);
using SurfaceDestroyed = void (*)();
using Frame = int32_t (*)();
using Touch = void (*)(int32_t, float, float, int64_t, float);

struct W3cosAbi {
  void *library = nullptr;
  SurfaceCreated surfaceCreated = nullptr;
  SurfaceChanged surfaceChanged = nullptr;
  SurfaceDestroyed surfaceDestroyed = nullptr;
  Frame frame = nullptr;
  Touch touch = nullptr;
};

W3cosAbi ABI;

bool ResolveAbi() {
  if (ABI.library != nullptr) {
    return ABI.surfaceCreated != nullptr;
  }
  ABI.library = dlopen("libw3cos_mobile_app.so", RTLD_NOW | RTLD_LOCAL);
  if (ABI.library == nullptr) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, LOG_DOMAIN, LOG_TAG,
                 "Unable to load libw3cos_mobile_app.so: %{public}s", dlerror());
    return false;
  }
  ABI.surfaceCreated = reinterpret_cast<SurfaceCreated>(
      dlsym(ABI.library, "w3cos_harmony_surface_created"));
  ABI.surfaceChanged = reinterpret_cast<SurfaceChanged>(
      dlsym(ABI.library, "w3cos_harmony_surface_changed"));
  ABI.surfaceDestroyed = reinterpret_cast<SurfaceDestroyed>(
      dlsym(ABI.library, "w3cos_harmony_surface_destroyed"));
  ABI.frame =
      reinterpret_cast<Frame>(dlsym(ABI.library, "w3cos_harmony_frame"));
  ABI.touch =
      reinterpret_cast<Touch>(dlsym(ABI.library, "w3cos_harmony_touch"));
  if (ABI.surfaceCreated == nullptr || ABI.surfaceChanged == nullptr ||
      ABI.surfaceDestroyed == nullptr || ABI.frame == nullptr ||
      ABI.touch == nullptr) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, LOG_DOMAIN, LOG_TAG,
                 "W3COS HarmonyOS ABI is incomplete");
    return false;
  }
  return true;
}

void OnSurfaceCreated(OH_NativeXComponent *component, void *window) {
  uint64_t width = 0;
  uint64_t height = 0;
  if (!ResolveAbi() ||
      OH_NativeXComponent_GetXComponentSize(component, window, &width, &height) !=
          OH_NATIVEXCOMPONENT_RESULT_SUCCESS) {
    return;
  }
  const int32_t result = ABI.surfaceCreated(
      window, static_cast<uint32_t>(width), static_cast<uint32_t>(height));
  OH_LOG_Print(LOG_APP, result == 0 ? LOG_INFO : LOG_ERROR, LOG_DOMAIN,
               LOG_TAG, "W3COS surface created: %{public}ux%{public}u result=%{public}d",
               static_cast<uint32_t>(width), static_cast<uint32_t>(height),
               result);
}

void OnSurfaceChanged(OH_NativeXComponent *component, void *window) {
  uint64_t width = 0;
  uint64_t height = 0;
  if (ResolveAbi() &&
      OH_NativeXComponent_GetXComponentSize(component, window, &width, &height) ==
          OH_NATIVEXCOMPONENT_RESULT_SUCCESS) {
    ABI.surfaceChanged(static_cast<uint32_t>(width),
                       static_cast<uint32_t>(height));
  }
}

void OnSurfaceDestroyed(OH_NativeXComponent *, void *) {
  if (ResolveAbi()) {
    ABI.surfaceDestroyed();
  }
}

void DispatchTouchEvent(OH_NativeXComponent *component, void *window) {
  OH_NativeXComponent_TouchEvent event;
  if (!ResolveAbi() ||
      OH_NativeXComponent_GetTouchEvent(component, window, &event) !=
          OH_NATIVEXCOMPONENT_RESULT_SUCCESS) {
    return;
  }
  const int32_t phase = static_cast<int32_t>(event.type);
  ABI.touch(phase, event.x, event.y, static_cast<int64_t>(event.id),
            event.force);
}

OH_NativeXComponent_Callback CALLBACK = {
    .OnSurfaceCreated = OnSurfaceCreated,
    .OnSurfaceChanged = OnSurfaceChanged,
    .OnSurfaceDestroyed = OnSurfaceDestroyed,
    .DispatchTouchEvent = DispatchTouchEvent,
};

napi_value FrameCallback(napi_env env, napi_callback_info) {
  int32_t result = ResolveAbi() ? ABI.frame() : 1;
  napi_value value = nullptr;
  napi_create_int32(env, result, &value);
  return value;
}

napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor properties[] = {
      {"frame", nullptr, FrameCallback, nullptr, nullptr, nullptr, napi_default,
       nullptr},
  };
  napi_define_properties(env, exports,
                         sizeof(properties) / sizeof(properties[0]), properties);

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
