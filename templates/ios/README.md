# iOS shell template (M2)

Generic app bundle and Xcode packaging shell generated from TSX via `w3cos mobile build`.

## Prerequisites

- **Xcode** (not Command Line Tools only): `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`
- Rust: `rustup target add aarch64-apple-ios-sim`
- Device evidence: `rustup target add aarch64-apple-ios`

## Build

From mobile project root:

```bash
w3cos mobile build --platform ios
```

Then open `ios/W3cosApp.xcodeproj` in Xcode → Run on iPhone simulator.

For an unsigned physical-device slice plus a timing/size report:

```bash
w3cos mobile build --platform ios --ios-target device --release \
  --report target/mobile-ios-device.json
```

The device slice still requires downstream signing and archive validation.

## Layout

```
ios/
├── W3cosApp.xcodeproj
├── W3cosApp/
│   ├── AppDelegate.swift
│   ├── ViewController.swift   # calls w3cos_app_run()
│   └── Info.plist
└── W3cosApp.app/              # generated simulator or unsigned device bundle
```

## Customize

Set `PRODUCT_BUNDLE_IDENTIFIER` in Xcode ↔ `bundle_id` in `w3cos.app.json`.
