# Recorded native dependency locks

These are the exact lockfiles used for the Linux x86-64 main-v2 versus Location
native-only experiment on 2026-10-12. They are inputs, not proof of portability
or evidence of a browser result. Do not silently update them during a comparison.

- workspace.Cargo.lock SHA256: 322f0ee708e19c252a491ad669cfe63f71a9807fa6636e8e423467feffcf4434
- native.Cargo.lock SHA256: dfa0ebfe65df977dee4bd409d1bd27d78ffaf0ecf600acf6b62ac5f9dbe12699

The workspace lock builds the CLI. The native lock belongs to the generated
w3cos-app project and is applied through the provenance-preserving locked rebuild
helper, not copied over the workspace lock. Native and browser do not share a
Cargo dependency graph; each engine's configuration is held fixed across pairs.
