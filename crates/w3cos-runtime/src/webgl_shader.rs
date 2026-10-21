//! WebGL GLSL ES compilation and program linking on the wgpu shader stack.
//!
//! WebGL shaders are GLSL ES 1.00 (WebGL 1 and 2) or GLSL ES 3.00 (WebGL 2).
//! wgpu's shader translator, `naga`, parses Vulkan-flavoured GLSL 450, so
//! [`compile`] first rewrites the WebGL dialect in a line-preserving way:
//!
//! - `#version 100`/`300 es` becomes `#version 450`; precision statements and
//!   qualifiers are removed;
//! - `attribute`/`varying`/`in`/`out` globals get explicit locations, and ES
//!   1.00 `gl_FragColor`/`gl_FragData[0]` become a location-0 output;
//! - loose uniforms get explicit bindings, and combined samplers
//!   (`sampler2D` …) are split into a texture plus a sampler that every use
//!   site recombines with `sampler2D(t, s)`;
//! - ES 1.00 texture lookups (`texture2D`, `textureCube` …) map to their
//!   GLSL 450 overloads.
//!
//! The rewritten source is parsed and validated by `naga`. Syntax, name and
//! type errors produce `COMPILE_STATUS = false` and a WebGL-style info log
//! (`ERROR: 0:<line>: <message>`) whose line numbers refer to the original
//! source. A construct `naga` reports as *not implemented* is accepted with a
//! `WARNING:` line instead of failing a shader a browser would compile.
//!
//! [`link`] checks attached stages, varyings statically used by the fragment
//! shader against vertex outputs, uniform type agreement, and assigns
//! attribute locations (honouring `bindAttribLocation`).

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
}

/// A declared shader interface variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub glsl_type: String,
    /// WebGL type enum (`FLOAT_VEC4`, `SAMPLER_2D`, …).
    pub gl_type: u32,
    /// Array length, 1 for non-arrays.
    pub size: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    /// Vertex attributes (vertex stage only).
    pub attributes: Vec<Variable>,
    /// Vertex outputs or fragment inputs.
    pub varyings: Vec<Variable>,
    /// Fragment-shader varyings that the shader body actually reads.
    pub used_varyings: Vec<String>,
    /// Non-opaque and sampler uniforms.
    pub uniforms: Vec<Variable>,
}

#[derive(Clone, Debug)]
pub struct CompiledShader {
    pub stage: Stage,
    pub success: bool,
    pub info_log: String,
    pub interface: Interface,
}

#[derive(Clone, Debug)]
pub struct LinkedProgram {
    pub success: bool,
    pub info_log: String,
    /// Attributes with their assigned locations.
    pub attributes: Vec<(Variable, u32)>,
    pub uniforms: Vec<Variable>,
}

/// GL enum for a GLSL type name, 0 if unknown.
pub fn gl_type(name: &str) -> u32 {
    match name {
        "float" => 0x1406,
        "vec2" => 0x8B50,
        "vec3" => 0x8B51,
        "vec4" => 0x8B52,
        "int" => 0x1404,
        "ivec2" => 0x8B53,
        "ivec3" => 0x8B54,
        "ivec4" => 0x8B55,
        "uint" => 0x1405,
        "uvec2" => 0x8DC6,
        "uvec3" => 0x8DC7,
        "uvec4" => 0x8DC8,
        "bool" => 0x8B56,
        "bvec2" => 0x8B57,
        "bvec3" => 0x8B58,
        "bvec4" => 0x8B59,
        "mat2" | "mat2x2" => 0x8B5A,
        "mat3" | "mat3x3" => 0x8B5B,
        "mat4" | "mat4x4" => 0x8B5C,
        "mat2x3" => 0x8B65,
        "mat2x4" => 0x8B66,
        "mat3x2" => 0x8B67,
        "mat3x4" => 0x8B68,
        "mat4x2" => 0x8B69,
        "mat4x3" => 0x8B6A,
        "sampler2D" => 0x8B5E,
        "sampler3D" => 0x8B5F,
        "samplerCube" => 0x8B60,
        "sampler2DShadow" => 0x8B62,
        "sampler2DArray" => 0x8DC1,
        "sampler2DArrayShadow" => 0x8DC4,
        "samplerCubeShadow" => 0x8DC5,
        "isampler2D" => 0x8DCA,
        "isampler3D" => 0x8DCB,
        "isamplerCube" => 0x8DCC,
        "isampler2DArray" => 0x8DCF,
        "usampler2D" => 0x8DD2,
        "usampler3D" => 0x8DD3,
        "usamplerCube" => 0x8DD4,
        "usampler2DArray" => 0x8DD7,
        _ => 0,
    }
}

/// (texture type, sampler type) for a combined GLSL ES sampler.
fn split_sampler(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "sampler2D" => ("texture2D", "sampler"),
        "sampler3D" => ("texture3D", "sampler"),
        "samplerCube" => ("textureCube", "sampler"),
        "sampler2DArray" => ("texture2DArray", "sampler"),
        "sampler2DShadow" => ("texture2D", "samplerShadow"),
        "samplerCubeShadow" => ("textureCube", "samplerShadow"),
        "sampler2DArrayShadow" => ("texture2DArray", "samplerShadow"),
        "isampler2D" => ("itexture2D", "sampler"),
        "isampler3D" => ("itexture3D", "sampler"),
        "isamplerCube" => ("itextureCube", "sampler"),
        "isampler2DArray" => ("itexture2DArray", "sampler"),
        "usampler2D" => ("utexture2D", "sampler"),
        "usampler3D" => ("utexture3D", "sampler"),
        "usamplerCube" => ("utextureCube", "sampler"),
        "usampler2DArray" => ("utexture2DArray", "sampler"),
        _ => return None,
    })
}

// ── Lexical helpers ───────────────────────────────────────────────────────

/// Replaces comments with spaces, keeping every newline in place.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                out.push(' ');
                index += 1;
            }
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            out.push_str("  ");
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                out.push(if bytes[index] == b'\n' { '\n' } else { ' ' });
                index += 1;
            }
            if index < bytes.len() {
                out.push_str("  ");
                index += 2;
            }
        } else {
            let ch = source[index..].chars().next().unwrap_or(' ');
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Replaces whole-identifier occurrences of `from` with `to`.
fn replace_ident(text: &str, from: &str, to: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while let Some(found) = text[index..].find(from) {
        let start = index + found;
        let end = start + from.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        out.push_str(&text[index..start]);
        if before_ok && after_ok {
            out.push_str(to);
        } else {
            out.push_str(from);
        }
        index = end;
    }
    out.push_str(&text[index..]);
    out
}

fn count_ident(text: &str, ident: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut index = 0;
    while let Some(found) = text[index..].find(ident) {
        let start = index + found;
        let end = start + ident.len();
        if (start == 0 || !is_ident_byte(bytes[start - 1]))
            && (end >= bytes.len() || !is_ident_byte(bytes[end]))
        {
            count += 1;
        }
        index = end;
    }
    count
}

/// Blanks `text` to spaces while keeping its newlines.
fn blank(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == '\n' { '\n' } else { ' ' })
        .collect()
}

/// Splits comment-free source into top-level segments. A segment ends at a
/// `;` at brace depth 0 or at a `}` that closes a function body. Uniform and
/// interface blocks (`uniform B { … } b;`) stay one segment.
fn top_level_segments(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut block_decl = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'{' => {
                if depth == 0 {
                    let head = source[start..index].trim_start();
                    block_decl = !head.contains('(')
                        || head.starts_with("layout")
                            && head
                                .split(')')
                                .next_back()
                                .is_some_and(|rest| !rest.contains('('));
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && !block_decl {
                    segments.push((start, index + 1));
                    start = index + 1;
                }
            }
            b';' if depth == 0 => {
                segments.push((start, index + 1));
                start = index + 1;
                block_decl = false;
            }
            b'\n' if depth == 0 && source[start..index].trim_start().starts_with('#') => {
                segments.push((start, index + 1));
                start = index + 1;
            }
            _ => {}
        }
    }
    if start < bytes.len() {
        segments.push((start, bytes.len()));
    }
    segments
}

// ── Declarations ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct Declaration {
    storage: String,
    interpolation: Vec<String>,
    explicit_location: Option<u32>,
    glsl_type: String,
    names: Vec<(String, u32, bool)>,
}

fn parse_declaration(text: &str) -> Option<Declaration> {
    let body = text.trim().strip_suffix(';')?.trim();
    let mut rest = body;
    let mut explicit_location = None;
    if let Some(after) = rest.strip_prefix("layout") {
        let open = after.find('(')?;
        let close = after.find(')')?;
        let inner = &after[open + 1..close];
        for item in inner.split(',') {
            let mut parts = item.split('=');
            if parts.next()?.trim() == "location" {
                explicit_location = parts.next().and_then(|value| value.trim().parse().ok());
            }
        }
        rest = after[close + 1..].trim_start();
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut index = 0;
    let mut interpolation = Vec::new();
    while let Some(word) = words.get(index) {
        if matches!(
            *word,
            "invariant" | "flat" | "smooth" | "centroid" | "noperspective"
        ) {
            interpolation.push(word.to_string());
            index += 1;
        } else {
            break;
        }
    }
    let storage = words.get(index)?.to_string();
    if !matches!(
        storage.as_str(),
        "attribute" | "varying" | "in" | "out" | "uniform"
    ) {
        return None;
    }
    index += 1;
    while words
        .get(index)
        .is_some_and(|word| matches!(*word, "highp" | "mediump" | "lowp"))
    {
        index += 1;
    }
    let glsl_type = words.get(index)?.to_string();
    if glsl_type.contains('{') || !glsl_type.bytes().all(is_ident_byte) {
        return None;
    }
    index += 1;
    let declarators = words[index..].join(" ");
    if declarators.contains('{') || declarators.contains('=') || declarators.contains('(') {
        return None;
    }
    let mut names = Vec::new();
    for declarator in declarators.split(',') {
        let declarator = declarator.trim();
        let (name, size, is_array) = match declarator.find('[') {
            Some(open) => {
                let size = declarator[open + 1..]
                    .trim_end_matches(']')
                    .trim()
                    .parse()
                    .unwrap_or(1);
                (declarator[..open].trim(), size, true)
            }
            None => (declarator, 1, false),
        };
        if name.is_empty() || !name.bytes().all(is_ident_byte) {
            return None;
        }
        names.push((name.to_string(), size, is_array));
    }
    Some(Declaration {
        storage,
        interpolation,
        explicit_location,
        glsl_type,
        names,
    })
}

fn declarator_text(name: &str, size: u32, is_array: bool) -> String {
    if is_array {
        format!("{name}[{size}]")
    } else {
        name.to_string()
    }
}

struct Translation {
    source: String,
    /// Lines prepended before the user's first line.
    line_offset: usize,
    interface: Interface,
}

fn translate(source: &str, stage: Stage, webgl2: bool) -> Result<Translation, String> {
    let clean = strip_comments(source);
    let mut es3 = false;
    let mut version_line = None;
    for (line_index, line) in clean.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("#version") {
            let rest = rest.trim();
            match rest {
                "100" => {}
                "300 es" if webgl2 => es3 = true,
                _ => {
                    return Err(format!(
                        "ERROR: 0:{}: '{rest}' : version number not supported",
                        line_index + 1
                    ));
                }
            }
            version_line = Some(line_index);
        }
        break;
    }

    let mut header = String::from("#version 450\n");
    if stage == Stage::Fragment && !es3 {
        header.push_str("layout(location=0) out vec4 _w3cos_FragColor;\n");
    }
    let mut body = clean.clone();
    if let Some(line_index) = version_line {
        let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
        lines[line_index] = blank(&lines[line_index]);
        body = lines.join("\n");
    }

    let mut interface = Interface::default();
    let mut samplers: Vec<(String, &'static str)> = Vec::new();
    let mut in_location = 0u32;
    let mut out_location = if stage == Stage::Fragment && !es3 {
        1
    } else {
        0
    };
    let mut binding = 0u32;
    let mut rewritten = String::with_capacity(body.len() + 256);
    let mut bodies = String::new();
    for (start, end) in top_level_segments(&body) {
        let segment = &body[start..end];
        let trimmed = segment.trim_start();
        if trimmed.starts_with("precision") && trimmed.trim_end().ends_with(';') {
            rewritten.push_str(&blank(segment));
            continue;
        }
        let Some(declaration) = parse_declaration(segment) else {
            if trimmed.starts_with("uniform") && segment.contains('{') {
                // Uniform block: give it a binding, keep members.
                let lead = segment.len() - trimmed.len();
                rewritten.push_str(&segment[..lead]);
                rewritten.push_str(&format!("layout(set=0,binding={binding}) "));
                binding += 1;
                rewritten.push_str(trimmed);
            } else {
                rewritten.push_str(segment);
                bodies.push_str(segment);
            }
            continue;
        };
        let leading_newlines = segment.matches('\n').count();
        let mut replacement = String::new();
        let qualifiers = declaration.interpolation.join(" ");
        let storage = match (stage, declaration.storage.as_str()) {
            (Stage::Vertex, "attribute") if !es3 => "in",
            (_, "attribute") => {
                return Err(
                    "ERROR: 0:1: 'attribute' : supported in vertex shaders of GLSL ES 1.00 only"
                        .into(),
                );
            }
            (Stage::Vertex, "varying") if !es3 => "out",
            (Stage::Fragment, "varying") if !es3 => "in",
            (_, "varying") => {
                return Err("ERROR: 0:1: 'varying' : not supported in GLSL ES 3.00".into());
            }
            (_, "in") | (_, "out") if !es3 => {
                return Err(format!(
                    "ERROR: 0:1: '{}' : storage qualifier supported in GLSL ES 3.00 only",
                    declaration.storage
                ));
            }
            (_, other) => {
                if other == "in" {
                    "in"
                } else if other == "out" {
                    "out"
                } else {
                    "uniform"
                }
            }
        };
        for (name, size, is_array) in &declaration.names {
            let is_array = *is_array;
            let variable = Variable {
                name: name.clone(),
                glsl_type: declaration.glsl_type.clone(),
                gl_type: gl_type(&declaration.glsl_type),
                size: *size,
            };
            match storage {
                "uniform" => {
                    if let Some((texture, sampler)) = split_sampler(&declaration.glsl_type) {
                        replacement.push_str(&format!(
                            "layout(set=1,binding={}) uniform {texture} {name}_w3cos_t; \
                             layout(set=1,binding={}) uniform {sampler} {name}_w3cos_s; ",
                            binding * 2,
                            binding * 2 + 1
                        ));
                        samplers.push((name.clone(), split_sampler_ctor(&declaration.glsl_type)));
                    } else {
                        replacement.push_str(&format!(
                            "layout(std140,set=0,binding={binding}) uniform _w3cos_u{binding} {{ {} {}; }}; ",
                            declaration.glsl_type,
                            declarator_text(name, *size, is_array)
                        ));
                    }
                    binding += 1;
                    interface.uniforms.push(variable);
                }
                "in" | "out" => {
                    let location = declaration.explicit_location.unwrap_or_else(|| {
                        let counter = if storage == "in" {
                            &mut in_location
                        } else {
                            &mut out_location
                        };
                        let location = *counter;
                        *counter += (*size).max(1) * location_slots(&declaration.glsl_type);
                        location
                    });
                    if let (Stage::Vertex, "in", Some((columns, rows)), false) = (
                        stage,
                        storage,
                        matrix_shape(&declaration.glsl_type),
                        is_array,
                    ) {
                        // naga rejects matrix entry-point inputs; feed the
                        // columns as vectors and rebuild the matrix by macro.
                        let mut parts = Vec::new();
                        for column in 0..columns {
                            header.push_str(&format!(
                                "layout(location={}) in vec{rows} {name}_w3cos_c{column};\n",
                                location + column
                            ));
                            parts.push(format!("{name}_w3cos_c{column}"));
                        }
                        header.push_str(&format!(
                            "#define {name} {}({})\n",
                            declaration.glsl_type,
                            parts.join(",")
                        ));
                        interface.attributes.push(variable);
                        continue;
                    }
                    replacement.push_str(&format!(
                        "layout(location={location}) {qualifiers} {storage} {} {}; ",
                        declaration.glsl_type,
                        declarator_text(name, *size, is_array)
                    ));
                    match (stage, storage) {
                        (Stage::Vertex, "in") => interface.attributes.push(variable),
                        (Stage::Vertex, "out") | (Stage::Fragment, "in") => {
                            interface.varyings.push(variable)
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        rewritten.push_str(&replacement);
        rewritten.push_str(&"\n".repeat(leading_newlines));
    }

    let mut text = rewritten;
    for (name, ctor) in &samplers {
        text = replace_ident(
            &text,
            name,
            &format!("{ctor}({name}_w3cos_t,{name}_w3cos_s)"),
        );
    }
    for qualifier in ["highp", "mediump", "lowp"] {
        text = replace_ident(&text, qualifier, "");
    }
    if !es3 {
        for (from, to) in [
            ("texture2DProjLodEXT", "textureProjLod"),
            ("texture2DLodEXT", "textureLod"),
            ("texture2DProjLod", "textureProjLod"),
            ("texture2DLod", "textureLod"),
            ("texture2DProj", "textureProj"),
            ("texture2D", "texture"),
            ("textureCubeLodEXT", "textureLod"),
            ("textureCubeLod", "textureLod"),
            ("textureCube", "texture"),
        ] {
            text = replace_ident(&text, from, to);
        }
        if stage == Stage::Fragment {
            text = text.replace("gl_FragData[0]", "_w3cos_FragColor");
            text = replace_ident(&text, "gl_FragColor", "_w3cos_FragColor");
        }
    }
    if stage == Stage::Fragment {
        interface.used_varyings = interface
            .varyings
            .iter()
            .filter(|varying| count_ident(&bodies, &varying.name) > 0)
            .map(|varying| varying.name.clone())
            .collect();
    }
    let line_offset = header.matches('\n').count();
    Ok(Translation {
        source: header + text.as_str(),
        line_offset,
        interface,
    })
}

fn split_sampler_ctor(name: &str) -> &'static str {
    match name {
        "sampler2D" => "sampler2D",
        "sampler3D" => "sampler3D",
        "samplerCube" => "samplerCube",
        "sampler2DArray" => "sampler2DArray",
        "sampler2DShadow" => "sampler2DShadow",
        "samplerCubeShadow" => "samplerCubeShadow",
        "sampler2DArrayShadow" => "sampler2DArrayShadow",
        "isampler2D" => "isampler2D",
        "isampler3D" => "isampler3D",
        "isamplerCube" => "isamplerCube",
        "isampler2DArray" => "isampler2DArray",
        "usampler2D" => "usampler2D",
        "usampler3D" => "usampler3D",
        "usamplerCube" => "usamplerCube",
        _ => "usampler2DArray",
    }
}

/// (columns, rows) of a matrix type.
fn matrix_shape(glsl_type: &str) -> Option<(u32, u32)> {
    let dims = glsl_type.strip_prefix("mat")?;
    let mut parts = dims.split('x').map(|part| part.parse::<u32>().ok());
    let columns = parts.next()??;
    let rows = parts.next().unwrap_or(Some(columns))?;
    Some((columns, rows))
}

fn location_slots(glsl_type: &str) -> u32 {
    match glsl_type {
        "mat2" | "mat2x2" | "mat2x3" | "mat2x4" => 2,
        "mat3" | "mat3x3" | "mat3x2" | "mat3x4" => 3,
        "mat4" | "mat4x4" | "mat4x2" | "mat4x3" => 4,
        _ => 1,
    }
}

// ── Compilation ───────────────────────────────────────────────────────────

/// Compiles one WebGL shader. `webgl2` enables `#version 300 es`.
pub fn compile(stage: Stage, source: &str, webgl2: bool) -> CompiledShader {
    let translation = match translate(source, stage, webgl2) {
        Ok(translation) => translation,
        Err(log) => {
            return CompiledShader {
                stage,
                success: false,
                info_log: log + "\n",
                interface: Interface::default(),
            };
        }
    };
    let (success, info_log) = check(stage, &translation);
    CompiledShader {
        stage,
        success,
        info_log,
        interface: translation.interface,
    }
}

#[cfg(feature = "web-graphics-advanced")]
fn check(stage: Stage, translation: &Translation) -> (bool, String) {
    use naga::front::glsl::{ErrorKind, Frontend, Options};

    let naga_stage = match stage {
        Stage::Vertex => naga::ShaderStage::Vertex,
        Stage::Fragment => naga::ShaderStage::Fragment,
    };
    let source = &translation.source;
    let line_of = |span: naga::Span| -> usize {
        let location = span.location(source);
        (location.line_number as usize)
            .saturating_sub(translation.line_offset)
            .max(1)
    };
    let mut frontend = Frontend::default();
    match frontend.parse(&Options::from(naga_stage), source) {
        Ok(module) => {
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            match validator.validate(&module) {
                Ok(_) => (true, String::new()),
                Err(error) => {
                    let line = error
                        .spans()
                        .map(|(span, _)| *span)
                        .filter(|span| span.is_defined())
                        .last()
                        .map(line_of)
                        .unwrap_or(1);
                    let mut message = error.as_inner().to_string();
                    let mut cause = std::error::Error::source(error.as_inner());
                    while let Some(inner) = cause {
                        message.push_str(": ");
                        message.push_str(&inner.to_string());
                        cause = inner.source();
                    }
                    (false, format!("ERROR: 0:{line}: {message}\n"))
                }
            }
        }
        Err(errors) => {
            let mut log = String::new();
            let mut fatal = false;
            for error in &errors.errors {
                let line = if error.meta.is_defined() {
                    line_of(error.meta)
                } else {
                    1
                };
                if matches!(error.kind, ErrorKind::NotImplemented(_)) {
                    log.push_str(&format!(
                        "WARNING: 0:{line}: not verified by the native shader translator: {}\n",
                        error.kind
                    ));
                } else {
                    fatal = true;
                    log.push_str(&format!("ERROR: 0:{line}: {}\n", error.kind));
                }
            }
            (!fatal, log)
        }
    }
}

#[cfg(not(feature = "web-graphics-advanced"))]
fn check(_stage: Stage, _translation: &Translation) -> (bool, String) {
    (
        true,
        "WARNING: 0:1: GLSL validation requires the web-graphics-advanced feature\n".into(),
    )
}

// ── Linking ───────────────────────────────────────────────────────────────

/// Links a vertex and fragment shader. `bound` holds `bindAttribLocation`
/// requests; `max_attribs` is `MAX_VERTEX_ATTRIBS`.
pub fn link(
    vertex: Option<&CompiledShader>,
    fragment: Option<&CompiledShader>,
    bound: &HashMap<String, u32>,
    max_attribs: u32,
) -> LinkedProgram {
    let fail = |log: String| LinkedProgram {
        success: false,
        info_log: log,
        attributes: Vec::new(),
        uniforms: Vec::new(),
    };
    let (Some(vertex), Some(fragment)) = (vertex, fragment) else {
        return fail("ERROR: Program must have a vertex and a fragment shader attached\n".into());
    };
    if !vertex.success || !fragment.success {
        return fail(
            "ERROR: Attached shaders must be compiled successfully before linking\n".into(),
        );
    }
    let mut log = String::new();
    for name in &fragment.interface.used_varyings {
        let input = fragment
            .interface
            .varyings
            .iter()
            .find(|varying| &varying.name == name);
        match vertex
            .interface
            .varyings
            .iter()
            .find(|varying| &varying.name == name)
        {
            None => log.push_str(&format!(
                "ERROR: Varying '{name}' is read by the fragment shader but not declared in the vertex shader\n"
            )),
            Some(output) => {
                if let Some(input) = input {
                    if input.glsl_type != output.glsl_type || input.size != output.size {
                        log.push_str(&format!(
                            "ERROR: Varying '{name}' has type {} in the vertex shader and {} in the fragment shader\n",
                            output.glsl_type, input.glsl_type
                        ));
                    }
                }
            }
        }
    }
    let mut uniforms: Vec<Variable> = vertex.interface.uniforms.clone();
    for uniform in &fragment.interface.uniforms {
        match uniforms
            .iter()
            .find(|existing| existing.name == uniform.name)
        {
            Some(existing)
                if existing.glsl_type != uniform.glsl_type || existing.size != uniform.size =>
            {
                log.push_str(&format!(
                    "ERROR: Uniform '{}' is declared with different types in the vertex and fragment shaders\n",
                    uniform.name
                ));
            }
            Some(_) => {}
            None => uniforms.push(uniform.clone()),
        }
    }
    let mut attributes = Vec::new();
    let mut used: Vec<u32> = Vec::new();
    for attribute in &vertex.interface.attributes {
        if let Some(location) = bound.get(&attribute.name) {
            let slots = location_slots(&attribute.glsl_type) * attribute.size.max(1);
            used.extend(*location..*location + slots);
            attributes.push((attribute.clone(), *location));
        }
    }
    for attribute in &vertex.interface.attributes {
        if bound.contains_key(&attribute.name) {
            continue;
        }
        let slots = location_slots(&attribute.glsl_type) * attribute.size.max(1);
        let mut location = 0;
        while (location..location + slots).any(|slot| used.contains(&slot)) {
            location += 1;
        }
        used.extend(location..location + slots);
        attributes.push((attribute.clone(), location));
    }
    if used.iter().any(|slot| *slot >= max_attribs) {
        log.push_str(&format!(
            "ERROR: Too many vertex attributes (MAX_VERTEX_ATTRIBS is {max_attribs})\n"
        ));
    }
    if !log.is_empty() {
        return fail(log);
    }
    attributes.sort_by_key(|(_, location)| *location);
    LinkedProgram {
        success: true,
        info_log: String::new(),
        attributes,
        uniforms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ES1_VS: &str = "attribute vec2 a_position;\n\
        attribute vec2 a_uv;\n\
        uniform mat4 u_matrix;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
          v_uv = a_uv;\n\
          gl_Position = u_matrix * vec4(a_position, 0.0, 1.0);\n\
        }\n";
    const ES1_FS: &str = "precision mediump float;\n\
        // sample the texture\n\
        uniform sampler2D u_texture;\n\
        uniform lowp vec4 u_tint;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
          gl_FragColor = texture2D(u_texture, v_uv) * u_tint;\n\
        }\n";

    #[test]
    fn webgl1_shaders_compile_and_link_with_interfaces() {
        let vs = compile(Stage::Vertex, ES1_VS, false);
        assert!(vs.success, "{}", vs.info_log);
        let fs = compile(Stage::Fragment, ES1_FS, false);
        assert!(fs.success, "{}", fs.info_log);
        assert_eq!(
            vs.interface
                .attributes
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["a_position", "a_uv"]
        );
        assert_eq!(fs.interface.used_varyings, ["v_uv"]);
        let program = link(Some(&vs), Some(&fs), &HashMap::new(), 16);
        assert!(program.success, "{}", program.info_log);
        assert_eq!(program.attributes[0].0.name, "a_position");
        assert_eq!(program.attributes[1].1, 1);
        let names: Vec<_> = program.uniforms.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["u_matrix", "u_texture", "u_tint"]);
        assert_eq!(program.uniforms[1].gl_type, 0x8B5E);
        assert_eq!(program.uniforms[0].gl_type, 0x8B5C);
    }

    #[test]
    #[cfg(feature = "web-graphics-advanced")]
    fn webgl2_es300_shaders_compile_and_require_webgl2() {
        let vs = "#version 300 es\nin vec3 position;\nuniform float scale[2];\nout highp vec3 v_color;\n\
                  void main() { v_color = position; gl_Position = vec4(position * scale[1], 1.0); }\n";
        let fs = "#version 300 es\nprecision highp float;\nin vec3 v_color;\nout vec4 outColor;\n\
                  void main() { outColor = vec4(v_color, 1.0); }\n";
        let compiled_vs = compile(Stage::Vertex, vs, true);
        assert!(compiled_vs.success, "{}", compiled_vs.info_log);
        assert_eq!(compiled_vs.interface.uniforms[0].size, 2);
        let compiled_fs = compile(Stage::Fragment, fs, true);
        assert!(compiled_fs.success, "{}", compiled_fs.info_log);
        assert!(link(Some(&compiled_vs), Some(&compiled_fs), &HashMap::new(), 16).success);
        let webgl1 = compile(Stage::Vertex, vs, false);
        assert!(!webgl1.success);
        assert!(
            webgl1.info_log.starts_with("ERROR: 0:1:"),
            "{}",
            webgl1.info_log
        );
    }

    #[test]
    #[cfg(feature = "web-graphics-advanced")]
    fn syntax_name_and_type_errors_fail_with_original_line_numbers() {
        let undeclared = compile(
            Stage::Fragment,
            "precision mediump float;\nvoid main() {\n  gl_FragColor = vec4(missing);\n}\n",
            false,
        );
        assert!(!undeclared.success);
        assert!(
            undeclared.info_log.starts_with("ERROR: 0:3:"),
            "{}",
            undeclared.info_log
        );
        assert!(
            undeclared.info_log.contains("missing"),
            "{}",
            undeclared.info_log
        );

        let syntax = compile(
            Stage::Vertex,
            "void main() {\n  gl_Position = vec4(1.0)\n}\n",
            false,
        );
        assert!(!syntax.success);
        assert!(
            syntax.info_log.starts_with("ERROR: 0:"),
            "{}",
            syntax.info_log
        );

        let mismatch = compile(
            Stage::Fragment,
            "precision mediump float;\nvoid main() {\n  vec3 c = vec3(1.0);\n  gl_FragColor = c;\n}\n",
            false,
        );
        assert!(!mismatch.success, "{}", mismatch.info_log);
        assert!(
            mismatch.info_log.starts_with("ERROR: 0:"),
            "{}",
            mismatch.info_log
        );

        let no_main = compile(Stage::Vertex, "void helper() {}\n", false);
        assert!(!no_main.success);
    }

    #[test]
    #[cfg(feature = "web-graphics-advanced")]
    fn link_rejects_missing_varyings_type_mismatches_and_unattached_stages() {
        let vs = compile(
            Stage::Vertex,
            "attribute vec4 p;\nvarying vec3 v_a;\nvoid main(){ v_a = p.xyz; gl_Position = p; }\n",
            false,
        );
        let fs_missing = compile(
            Stage::Fragment,
            "precision mediump float;\nvarying vec2 v_b;\nvoid main(){ gl_FragColor = vec4(v_b, 0.0, 1.0); }\n",
            false,
        );
        assert!(vs.success && fs_missing.success);
        let linked = link(Some(&vs), Some(&fs_missing), &HashMap::new(), 16);
        assert!(!linked.success);
        assert!(linked.info_log.contains("v_b"), "{}", linked.info_log);

        let fs_type = compile(
            Stage::Fragment,
            "precision mediump float;\nvarying vec2 v_a;\nvoid main(){ gl_FragColor = vec4(v_a, 0.0, 1.0); }\n",
            false,
        );
        assert!(!link(Some(&vs), Some(&fs_type), &HashMap::new(), 16).success);

        let fs_unused = compile(
            Stage::Fragment,
            "precision mediump float;\nvarying vec2 v_unused;\nvoid main(){ gl_FragColor = vec4(1.0); }\n",
            false,
        );
        assert!(link(Some(&vs), Some(&fs_unused), &HashMap::new(), 16).success);
        assert!(!link(Some(&vs), None, &HashMap::new(), 16).success);
    }

    #[test]
    fn bound_attribute_locations_are_honoured_and_matrices_take_slots() {
        let vs = compile(
            Stage::Vertex,
            "attribute mat4 m;\nattribute vec4 p;\nvoid main(){ gl_Position = m * p; }\n",
            false,
        );
        let fs = compile(
            Stage::Fragment,
            "void main(){ gl_FragColor = vec4(1.0); }\n",
            false,
        );
        assert!(vs.success, "{}", vs.info_log);
        assert!(fs.success, "{}", fs.info_log);
        let bound = HashMap::from([("p".to_string(), 0)]);
        let linked = link(Some(&vs), Some(&fs), &bound, 16);
        assert!(linked.success, "{}", linked.info_log);
        assert_eq!(
            linked.attributes[0],
            (vs.interface.attributes[1].clone(), 0)
        );
        assert_eq!(linked.attributes[1].1, 1);
        assert!(!link(Some(&vs), Some(&fs), &HashMap::new(), 4).success);
    }

    #[test]
    fn comment_and_identifier_rewrites_are_boundary_safe() {
        assert_eq!(strip_comments("a/*x\ny*/b // c\nd"), "a   \n   b     \nd");
        assert_eq!(
            replace_ident("tex texture tex2 (tex)", "tex", "T"),
            "T texture tex2 (T)"
        );
        assert_eq!(count_ident("v_uv v_uv2 v_uv", "v_uv"), 2);
    }
}
