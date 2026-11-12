//! Generate a split bundle (see `esm_codegen::SplitBundle`) as a cargo
//! workspace, optionally building the segments that hold hot modules at a
//! higher `opt-level`.
//!
//! ```text
//! cargo run -p w3cos-compiler --example split_workspace -- \
//!     --root <project dir> --entry out/main.js --out <workspace dir> \
//!     [--segment-mb 8] [--dylib] [--mimalloc] \
//!     [--hot-modules hot.txt]                 # module path suffixes, one per line
//!     [--hot-from-stacks stacks.txt --hot-fraction 0.8 --hot-write hot.txt]
//!     [--hot-opt-level 1] [--hot-profile dev]
//! ```
//!
//! The hot list names modules, not segments, so it survives regeneration.
//!
//! `--mimalloc` links mimalloc (MIT) into the executable as the process-wide
//! `malloc`; see `split_profile::MIMALLOC_DEPENDENCY` for why that is the
//! allocator arrangement that is safe for a dylib split.
//! `--hot-from-stacks` ranks modules from symbolized samples (see
//! `scripts/symbolize-prof-stacks.py`); the stacks must come from a build of
//! the *same* generation, because they name modules by `mN` namespace.

use std::path::PathBuf;
use w3cos_compiler::esm_codegen::try_generate_split_with_bodies_and_stylesheet;
use w3cos_compiler::esm_resolver::{EsmBundle, EsmResolver};
use w3cos_compiler::split_profile::{
    HotModules, allocator_dependency, allocator_main_prelude, allocator_profile,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut opts = std::collections::HashMap::<String, String>::new();
    while let Some(arg) = args.next() {
        let key = arg.trim_start_matches("--").to_string();
        if key == "dylib" || key == "mimalloc" {
            opts.insert(key, String::new());
        } else {
            opts.insert(
                key,
                args.next().unwrap_or_else(|| panic!("{arg} needs a value")),
            );
        }
    }
    let opt = |key: &str| opts.get(key).cloned();
    let root = PathBuf::from(opt("root").expect("--root <project dir>"));
    let entry = root.join(opt("entry").unwrap_or_else(|| "out/main.js".into()));
    let out = PathBuf::from(opt("out").expect("--out <workspace dir>"));
    let segment_bytes =
        opt("segment-mb").map_or(8, |v| v.parse().expect("--segment-mb")) * 1_000_000;
    let dylib = opts.contains_key("dylib");
    let mimalloc = opts.contains_key("mimalloc");
    let opt_level: u8 = opt("hot-opt-level").map_or(1, |v| v.parse().expect("--hot-opt-level"));
    let profile = opt("hot-profile").unwrap_or_else(|| "dev".into());

    let resolver = EsmResolver::new(&root);
    let graph = resolver.build_graph_from_entry(&entry).expect("graph");
    let css = w3cos_compiler::esm_css::collect_esm_css(&graph, &resolver);
    let parsed = resolver.parse_graph_from_entry(&entry).expect("graph");
    let bundle = EsmBundle::build(&parsed, &resolver, &entry);
    let split = try_generate_split_with_bodies_and_stylesheet(&bundle, &css, segment_bytes)
        .expect("codegen");
    println!(
        "modules={} segments={}",
        bundle.modules.len(),
        split.segments.len()
    );

    let mut hot = opt("hot-modules")
        .map(|path| HotModules::parse(&std::fs::read_to_string(path).expect("--hot-modules")))
        .unwrap_or_default();
    if let Some(stacks) = opt("hot-from-stacks") {
        let fraction = opt("hot-fraction").map_or(0.8, |v| v.parse().expect("--hot-fraction"));
        let stacks = std::fs::read_to_string(stacks).expect("--hot-from-stacks");
        let ranked = HotModules::from_symbolized_stacks(&stacks, &split, fraction);
        // Store paths relative to the project so the list survives a move.
        let ranked = HotModules::new(ranked.entries().iter().map(|entry| {
            std::path::Path::new(entry)
                .strip_prefix(&root)
                .map_or(entry.clone(), |rel| rel.to_string_lossy().into_owned())
        }));
        hot = HotModules::new(hot.entries().iter().chain(ranked.entries()).cloned());
        if let Some(path) = opt("hot-write") {
            std::fs::write(&path, hot.to_text()).expect("--hot-write");
            println!("wrote {} hot modules to {path}", hot.entries().len());
        }
    }
    let hot_segments = split.hot_segments(&hot).count();
    println!(
        "hot modules={} hot segments={hot_segments}",
        hot.entries().len()
    );

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let core = workspace
        .join("crates/w3cos-core")
        .canonicalize()
        .expect("w3cos-core");
    let rt = workspace
        .join("crates/w3cos-runtime")
        .canonicalize()
        .expect("w3cos-runtime");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(out.join("src")).unwrap();
    let mut members = Vec::new();
    let mut root_deps = String::new();
    if dylib {
        let dir = out.join("segments/w3cos_base");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"w3cos_base\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\ncrate-type = [\"dylib\"]\n\n[dependencies]\nw3cos-core = {{ path = {core:?} }}\nw3cos-runtime = {{ path = {rt:?}, default-features = false }}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub use w3cos_core;\npub use w3cos_runtime;\n",
        )
        .unwrap();
        members.push("\"segments/w3cos_base\"".to_string());
        root_deps.push_str("w3cos_base = { path = \"segments/w3cos_base\" }\n");
    }
    for segment in &split.segments {
        let dir = out.join("segments").join(&segment.name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let mut deps = format!(
            "w3cos-core = {{ path = {core:?} }}\nw3cos-runtime = {{ path = {rt:?}, default-features = false }}\n"
        );
        if dylib {
            deps.push_str("w3cos_base = { path = \"../w3cos_base\" }\n");
        }
        for dep in &segment.deps {
            deps.push_str(&format!("{dep} = {{ path = \"../{dep}\" }}\n"));
        }
        let lib = if dylib {
            "[lib]\ncrate-type = [\"dylib\"]\n\n"
        } else {
            ""
        };
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n{lib}[dependencies]\n{deps}",
                segment.name
            ),
        )
        .unwrap();
        let code = if dylib {
            format!("{}\nextern crate w3cos_base;\n", segment.code)
        } else {
            segment.code.clone()
        };
        std::fs::write(dir.join("src/lib.rs"), code).unwrap();
        members.push(format!("\"segments/{}\"", segment.name));
        root_deps.push_str(&format!(
            "{} = {{ path = \"segments/{}\" }}\n",
            segment.name, segment.name
        ));
    }
    let overrides = split.profile_overrides(&hot, &profile, opt_level);
    std::fs::write(
        out.join("Cargo.toml"),
        format!(
            "[package]\nname = \"w3cos-split-app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {core:?} }}\nw3cos-runtime = {{ path = {rt:?}, default-features = false }}\n{root_deps}{alloc_dep}\n[workspace]\nmembers = [{}]\n\n[profile.dev]\ndebug = 0\n{overrides}{alloc_profile}",
            members.join(", "),
            alloc_dep = allocator_dependency(mimalloc),
            alloc_profile = allocator_profile(mimalloc)
        ),
    )
    .unwrap();
    std::fs::write(out.join("src/esm_bundle.rs"), &split.root).unwrap();
    std::fs::write(
        out.join("src/main.rs"),
        format!(
            "{}{}mod esm_bundle;\nfn main() {{ let v = esm_bundle::run_entry(); println!(\"run_entry -> {{:?}}\", v.to_js_string()); }}\n",
            allocator_main_prelude(mimalloc),
            if dylib { "extern crate w3cos_base;\n" } else { "" }
        ),
    )
    .unwrap();
}
