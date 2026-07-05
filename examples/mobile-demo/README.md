# mobile-demo

Generic W3C OS **mobile** example — validates the mobile shell pipeline (not tied to any downstream product).

## Desktop (dev)

Same TSX as other examples; uses desktop window until Android NDK backend is complete:

```bash
w3cos build examples/mobile-demo/app.tsx -o mobile-demo --release
./mobile-demo
```

## Mobile (M1+)

```bash
w3cos mobile init MyApp --platform android
# or use templates/android/ directly — see docs/MOBILE.md
```

## Manifest

`w3cos.app.json` describes bundle id, entry, and shell chrome. Parsed by `w3cos-mobile` crate.
