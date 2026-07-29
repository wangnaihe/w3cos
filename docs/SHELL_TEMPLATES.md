# Cross-platform Shell Templates

## Purpose

W3COS needs a consistent way to package the portable runtime and Shell services
for every supported operating system. A platform template is a thin native host:
it owns the operating-system lifecycle, native surfaces, packaging, and
permissions, while shared W3COS code owns DOM/layout, commands, notifications,
context, AI sessions, and portable presentation.

This plan uses **Shell** for the portable service and presentation layer and
**Host** for the native platform wrapper. Linux is the only target that acts as
the operating-system Shell by default. Windows and macOS are normal desktop
application hosts, and Android, iOS, and HarmonyOS are single-application mobile
hosts.

## Design constraints

1. Templates contain no example products, business navigation, or mocked system
   data.
2. Native code is limited to lifecycle, surfaces, input, permissions,
   accessibility, operating-system integration, and packaging.
3. All hosts implement a versioned Host ABI and declare capabilities at
   runtime. An unavailable operation returns a typed unsupported result.
4. Generated projects are reproducible and can be updated without overwriting
   application-owned files.
5. Platform identifiers, bundle metadata, icons, permissions, and entry points
   come from `w3cos.app.json`; duplicated native values are generated or
   validated.
6. Debug-only facilities such as DevTools are opt-in for release artifacts.
7. Every template has a build check that does not require signing, plus a
   platform smoke test where CI runners are available.

## Architecture

```text
Application TS/TSX + w3cos.app.json
                 |
        portable Shell presentation
                 |
          w3cos-shell-core
  state, commands, notifications, context,
  permissions, persistence, typed protocols
                 |
        versioned w3cos-host ABI
                 |
   +-------------+-------------+-------------+
   |             |             |             |
 Linux       Windows/macOS  Android/iOS   HarmonyOS
 system Host  desktop Hosts  mobile Hosts   mobile Host
   |             |             |             |
 compositor/   native app/   Activity/     Ability/
 session       window APIs   Scene APIs     XComponent
```

The Host ABI is narrower than the Shell service protocol:

- `w3cos.shell`: typed application-facing services such as launch, window,
  command, notification, context, and action outcome.
- `w3cos.host`: Shell-to-native operations such as surface, input, lifecycle,
  activation, permissions, dialogs, clipboard, accessibility, and power.

Neither protocol exposes platform handles to portable application code.

## Proposed repository layout

```text
crates/
  w3cos-shell-core/          # platform-neutral state and service protocols
  w3cos-host/                # Host ABI, capabilities, test fixtures
  w3cos-host-linux/          # session/compositor adapter
  w3cos-host-windows/        # Win32 adapter
  w3cos-host-macos/          # AppKit adapter
  w3cos-mobile/              # shared mobile runtime adapter
  w3cos-shell/               # Linux system Shell composition and binary

templates/
  shell/
    common/                  # generated metadata and shared template assets
    linux/                   # system session package
    windows/                 # Win32 desktop package
    macos/                   # AppKit desktop package
    android/                 # Activity/NativeActivity package
    ios/                     # UIKit Scene package
    harmony/                 # ArkUI/XComponent package
  shared/                    # compatibility during migration
  android/                   # compatibility source during migration
  ios/                       # compatibility source during migration
  harmony/                   # compatibility source during migration

tests/
  shell-fixtures/            # platform-neutral protocol/capability fixtures
  shell-smoke/               # launch, activation, lifecycle, and surface tests
```

Existing `templates/android`, `templates/ios`, and `templates/harmony` remain
the source of `w3cos mobile init` until the new generator is stable. They then
become compatibility aliases or are removed in a breaking release; they must
not be copied and maintained in two locations.

## Template contract

Every platform directory has the same conceptual inputs and outputs:

```text
Inputs
  w3cos.app.json
  application native library/binary
  icons and launch assets
  optional native extensions

Required template-owned files
  template.toml             # template version, target, and required tools
  capabilities.toml         # implemented, conditional, unsupported
  generated-files.toml      # ownership and merge policy
  README.md                 # prerequisites, build, run, signing

Outputs
  unsigned deb/rpm/image, exe/msix, app/dmg,
  apk/aab, app/xcarchive, or hap as applicable
```

`template.toml` should include:

- template and minimum Host ABI versions;
- supported architectures and minimum OS versions;
- debug/release commands and artifact paths;
- toolchain probes;
- signing mode (`none`, `local`, or `distribution`);
- files that can be regenerated safely.

`capabilities.toml` is both documentation and test input. Initial capability
names should cover:

- surface and multi-window;
- pointer, touch, keyboard, IME, and back navigation;
- safe area, display scale, orientation, and multi-display;
- lifecycle and state restoration;
- deep link, file open, share, and protocol activation;
- notifications, badges, tray/status item, and global shortcuts;
- clipboard, file picker, and drag/drop;
- permissions and accessibility;
- power, lock/session, and process supervision.

## Platform plans

### Linux system Host

**Template:** `templates/shell/linux`

- Package the `w3cos-shell` session, `.desktop`/display-manager entry, service
  files, rootfs overlay, and icon/theme assets.
- Use Wayland as the primary compositor/session path and keep explicit X11
  compatibility boundaries.
- Own application processes, windows, workspaces, notifications, status
  services, lock/power actions, and session recovery.
- Preserve the existing Buildroot ISO path, but make it consume the same
  generated package metadata as a normal Linux installation.
- Build targets: x86_64 and aarch64; unsigned package and QEMU boot smoke test.

### Windows desktop Host

**Template:** `templates/shell/windows`

- Use a normal Win32 desktop application by default; custom-shell/kiosk mode is
  a separate opt-in profile.
- Implement window lifecycle, DPI awareness, multi-display, taskbar/tray, jump
  lists, notifications, file associations, protocol activation, global
  shortcuts, clipboard, dialogs, and UI Automation.
- Keep application manifest, resources, icons, MSIX metadata, and optional
  installer definitions generated from the W3COS manifest.
- Build targets: x86_64-pc-windows-msvc first, aarch64 second; unsigned EXE/MSIX
  and launch/activation smoke tests.

### macOS desktop Host

**Template:** `templates/shell/macos`

- Use `NSApplication`/`NSWindow`; do not replace Finder or `loginwindow`.
- Integrate the global menu bar, Dock, notifications, file/protocol activation,
  fullscreen/Spaces, Retina scaling, accessibility, clipboard, dialogs, and
  sandbox permissions.
- Generate `Info.plist`, entitlements, asset catalog, and Xcode project/build
  configuration from the W3COS manifest.
- Build arm64 first and x86_64 where supported; produce an unsigned `.app` for
  CI, then add signing/notarization as a separate distribution step.

### Android mobile Host

**Template:** `templates/shell/android`

- Evolve the existing NativeActivity template without embedding portable Shell
  UI in Kotlin/Java.
- Cover `Surface` lifecycle, edge-to-edge content, display cutouts, system bars,
  predictive back, intents/deep links/share, runtime permissions,
  notifications, IME, accessibility, foreground/background transitions, and
  process recreation.
- Generate Gradle namespace/application ID, manifest entries, resources, and
  permission declarations from `w3cos.app.json`.
- Build ABI splits for arm64-v8a first, then x86_64 emulator and other declared
  ABIs; produce unsigned APK/AAB and run emulator lifecycle tests.

### iOS/iPadOS mobile Host

**Template:** `templates/shell/ios`

- Move from the current AppDelegate-only skeleton to UIKit Scene lifecycle.
- Cover safe areas, status bar/home indicator, URL/document activation, share,
  permissions, notifications, IME, accessibility, state restoration, and
  opt-in iPad multi-window.
- Generate bundle settings, `Info.plist`, entitlements, privacy usage strings,
  asset catalog, and Xcode build settings from `w3cos.app.json`.
- Build simulator arm64 first and device arm64 without signing in CI; archive,
  signing, and export remain explicit distribution steps.

### HarmonyOS mobile Host

**Template:** `templates/shell/harmony`

- Continue using a native ArkUI `XComponent`; do not use Android compatibility.
- Complete `OHNativeWindow` rendering, input, lifecycle, IME, safe areas,
  accessibility, permissions, notifications, and Ability restoration before
  advertising production support.
- Generate bundle/module metadata, permissions, resources, and signing-profile
  placeholders from `w3cos.app.json`.
- Build arm64-v8a first; require a real HAP plus emulator/device surface and
  lifecycle validation before graduating from experimental status.

## Manifest evolution

Keep portable values at the top level and place exceptions under a namespaced
platform object:

```json
{
  "schema_version": 2,
  "name": "MyApp",
  "bundle_id": "com.example.myapp",
  "entry": "app.tsx",
  "shell": {
    "mode": "application",
    "content_slot": "root"
  },
  "capabilities": [
    "clipboard.read",
    "notifications"
  ],
  "platforms": {
    "linux": { "session": false },
    "windows": { "kiosk": false },
    "macos": { "category": "public.app-category.utilities" },
    "android": { "min_sdk": 26 },
    "ios": { "minimum_version": "16.0", "ipad_multi_window": false },
    "harmony": { "api_version": 12 }
  }
}
```

Platform sections may refine packaging and operating-system declarations, but
must not change application behavior. Permissions are generated from the
capability list and fail validation when required human-readable descriptions
are absent.

## CLI and update model

The final command family should work for desktop and mobile:

```bash
w3cos shell init MyApp --platform android
w3cos shell add --platform windows
w3cos shell doctor --platform ios
w3cos shell build --platform macos --release
w3cos shell update --platform all
w3cos shell diff --platform android
```

During migration, `w3cos mobile init/build/dev` remains supported and delegates
to the same platform registry. `both` stays an Android+iOS compatibility alias;
new code uses a repeatable `--platform` or `--platform all`.

Template updates use three ownership classes:

- **generated:** safely replaced from manifest inputs;
- **mergeable:** updated through stable marked sections;
- **user-owned:** never overwritten; `shell diff` reports suggested changes.

Each generated project records template version and input digest in
`.w3cos/shell-lock.json`. A Host ABI mismatch is detected before building.

## Delivery sequence

### T0 — Contract and inventory

- Freeze terminology and target identifiers.
- Inventory every existing Android/iOS/Harmony template file and its owner.
- Define `template.toml`, `capabilities.toml`, the Host ABI version rules, and
  golden manifest fixtures.
- Gate: schema tests pass without moving existing templates.

### T1 — Mobile template normalization

- Put Android, iOS, and HarmonyOS behind one platform registry in the CLI.
- Generate duplicated bundle metadata from `w3cos.app.json`.
- Add `doctor`, artifact discovery, and unsigned build checks.
- Gate: existing `mobile init/build` behavior remains compatible and golden
  generated trees pass on all three platforms.

### T2 — Desktop application Hosts

- Add Windows and macOS templates plus thin Host adapters.
- Extract reusable Shell contracts into `w3cos-shell-core` and `w3cos-host`.
- Gate: the same sample opens a surface, accepts input, handles activation, and
  exits cleanly on Windows and macOS.

### T3 — Linux system Host migration

- Move Linux session/compositor integration behind the Host ABI.
- Make Buildroot and normal Linux packaging consume the Linux template.
- Gate: two registered apps can be launched, focused, resized, closed, and
  restored after a Shell restart in QEMU.

### T4 — Conformance and distribution

- Run shared lifecycle, capability, command, notification, permission,
  accessibility, and restoration fixtures across every Host.
- Add architecture matrix, artifact size tracking, signing documentation, and
  release packaging.
- Gate: unsupported capabilities are declared and tested; no platform silently
  renders a control that cannot work.

## Test matrix

| Target | Compile/package gate | Automated smoke gate | Later distribution gate |
|---|---|---|---|
| Linux x86_64/aarch64 | package + ISO | headless compositor/QEMU | signed repository/image |
| Windows x86_64/aarch64 | EXE + MSIX | launch, activation, DPI | code signing/store |
| macOS arm64/x86_64 | unsigned `.app` | launch, URL/file open | signing/notarization |
| Android arm64/x86_64 | APK + AAB | emulator lifecycle/input | signing/Play validation |
| iOS simulator/device | `.app` + archive | simulator lifecycle/input | signing/App Store export |
| HarmonyOS arm64 | HAP | emulator/device lifecycle | signing/store validation |

Cross-platform golden tests must validate identical protocol outcomes for
portable operations and explicit capability differences for native operations.

## Initial definition of done

The template program is usable when:

1. one manifest can generate all declared platform projects;
2. regeneration never overwrites a user-owned file;
3. each host reports its Host ABI and capability set;
4. each target can produce an unsigned CI artifact;
5. a shared fixture launches, presents a surface, processes input and lifecycle
   events, and shuts down cleanly;
6. Android/iOS/Harmony compatibility commands still work through the unified
   registry; and
7. platform-specific product UI does not exist in the templates.
