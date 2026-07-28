//! Incremental SWC → W3IR lowering for runtime-loaded scripts.
//!
//! Unsupported syntax is rejected explicitly. The initial vertical slice
//! covers the browser-facing syntax needed by runtime-loaded scripts while
//! preserving one backend-neutral control-flow representation.

use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet};
use swc_common::{DUMMY_SP, FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::{
    AssignExpr, AssignOp, AssignTarget, BinaryOp, BlockStmt, BlockStmtOrExpr, Callee, Class,
    ClassMember, Decl, Expr, ExprOrSpread, ExprStmt, ForHead, Lit, MemberExpr, MemberProp,
    MethodKind, ModuleDecl, ModuleExportName, ModuleItem, OptChainBase, ParamOrTsParamProp, Pat,
    Prop, PropName, PropOrSpread, SimpleAssignTarget, Stmt, SuperProp, SuperPropExpr, ThisExpr,
    TsParamPropParam, UnaryOp, UpdateOp, VarDeclKind,
};
use swc_ecma_ast::{ExportSpecifier, ImportSpecifier};
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax};
use w3cos_ir::{
    BinaryOperator, Binding, BindingId, BindingKind, Block, BlockId, Constant, ExceptionRegion,
    Export as IrExport, Function, FunctionId, GeneratorSuspensionPoint, Import, Instruction,
    Module, Register, SuspensionId, SuspensionPoint, UnaryOperator,
};

pub fn lower_script(source: &str, specifier: &str) -> Result<Module> {
    let parsed = parse(source, specifier)?;
    let annex_b_function_declarations = !module_items_have_use_strict_directive(&parsed.body);
    let mut builder = Builder::entry(false, annex_b_function_declarations);
    for item in &parsed.body {
        if let ModuleItem::Stmt(statement) = item {
            builder.predeclare_function_binding(statement)?;
        }
    }
    for item in &parsed.body {
        if let ModuleItem::Stmt(statement) = item {
            builder.predeclare_annex_b_branch_functions(statement)?;
        }
    }
    for item in &parsed.body {
        if let ModuleItem::Stmt(statement) = item {
            builder.predeclare_var_bindings(statement)?;
        }
    }
    for item in &parsed.body {
        if let ModuleItem::Stmt(statement) = item {
            builder.predeclare_lexical_binding(statement)?;
        }
    }
    for item in &parsed.body {
        if let ModuleItem::Stmt(statement) = item {
            builder.initialize_function_declaration(statement)?;
        }
    }
    for item in &parsed.body {
        match item {
            ModuleItem::Stmt(statement) => builder.lower_statement(statement)?,
            ModuleItem::ModuleDecl(_) => {
                return Err(anyhow!(
                    "static ESM declarations are not accepted by classic script lowering"
                ));
            }
        }
    }
    finish_module(builder, specifier, "<script>")
}

/// Lower already-parsed module-evaluation statements into a standalone W3IR
/// entry. Unresolved identifiers become `w3cos:global` imports so native ESM
/// codegen can adapt them to its existing live module cells. This lets the
/// incremental ESM pipeline reuse W3IR without printing and reparsing SWC AST.
pub(crate) fn lower_module_statements(
    statements: &[Stmt],
    specifier: &str,
    external_bindings: &[(String, bool)],
) -> Result<Module> {
    let mut builder = Builder::entry(true, false);
    for (name, mutable) in external_bindings {
        builder.declare_external(name, *mutable)?;
    }
    for statement in statements {
        builder.predeclare_function_binding(statement)?;
    }
    for statement in statements {
        builder.predeclare_var_bindings(statement)?;
    }
    for statement in statements {
        builder.predeclare_lexical_binding(statement)?;
    }
    for statement in statements {
        builder.initialize_function_declaration(statement)?;
    }
    for statement in statements {
        builder.lower_statement(statement)?;
    }
    let mut module = finish_module(builder, specifier, "<module-init>")?;
    prune_unused_module_init_externals(&mut module);
    module.validate().map_err(|error| anyhow!(error))?;
    Ok(module)
}

fn prune_unused_module_init_externals(module: &mut Module) {
    let external_bindings = module
        .imports
        .iter()
        .filter(|import| import.specifier == "w3cos:external" || import.specifier == "w3cos:global")
        .map(|import| import.local)
        .collect::<HashSet<_>>();
    let mut used = HashSet::new();
    for function in &module.functions {
        used.extend(function.captures.iter().copied());
        for block in &function.blocks {
            for instruction in &block.instructions {
                match instruction {
                    Instruction::LoadBinding { binding, .. }
                    | Instruction::InitializeBinding { binding, .. }
                    | Instruction::StoreBinding { binding, .. }
                    | Instruction::RefreshBinding { binding } => {
                        used.insert(*binding);
                    }
                    Instruction::CreateClosure { captures, .. } => {
                        used.extend(captures.iter().copied());
                    }
                    _ => {}
                }
            }
        }
    }
    let unused = external_bindings
        .difference(&used)
        .copied()
        .collect::<HashSet<_>>();
    if unused.is_empty() {
        return;
    }
    module
        .imports
        .retain(|import| !unused.contains(&import.local));
    for function in &mut module.functions {
        function
            .bindings
            .retain(|binding| !unused.contains(&binding.id));
    }
}

/// Lowers an ECMAScript module to the same W3IR used by classic scripts.
/// Static imports become live lexical bindings; no separate module engine is
/// introduced.
pub fn lower_module(source: &str, specifier: &str) -> Result<Module> {
    let parsed = parse(source, specifier)?;
    let mut builder = Builder::entry(true, false);

    // Instantiate imports before evaluating declarations, matching the ESM
    // split between linking and execution.
    for (item_index, item) in parsed.body.iter().enumerate() {
        match item {
            ModuleItem::Stmt(statement) => {
                builder.predeclare_function_binding(statement)?;
                builder.predeclare_var_bindings(statement)?;
                builder.predeclare_lexical_binding(statement)?;
            }
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) if !import.type_only => {
                let source = atom_to_string(&import.src.value);
                builder.request_module(&source);
                for specifier in &import.specifiers {
                    if specifier.is_type_only() {
                        continue;
                    }
                    let (local, imported) = match specifier {
                        ImportSpecifier::Named(named) => (
                            named.local.sym.to_string(),
                            named
                                .imported
                                .as_ref()
                                .map(module_export_name)
                                .unwrap_or_else(|| named.local.sym.to_string()),
                        ),
                        ImportSpecifier::Default(default) => {
                            (default.local.sym.to_string(), "default".into())
                        }
                        ImportSpecifier::Namespace(namespace) => {
                            (namespace.local.sym.to_string(), "*".into())
                        }
                    };
                    builder.declare_import(&local, &source, &imported)?;
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export))
                if is_runtime_erased_declaration(&export.decl) => {}
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::Var(declaration) => {
                    builder.predeclare_var_bindings(&Stmt::Decl(Decl::Var(declaration.clone())))?;
                    builder
                        .predeclare_lexical_binding(&Stmt::Decl(Decl::Var(declaration.clone())))?;
                }
                Decl::Fn(declaration) => {
                    builder
                        .predeclare_function_binding(&Stmt::Decl(Decl::Fn(declaration.clone())))?;
                }
                Decl::Class(declaration) => {
                    builder.predeclare_lexical_binding(&Stmt::Decl(Decl::Class(
                        declaration.clone(),
                    )))?;
                }
                _ => {}
            },
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(export))
                if export.src.is_some() && !export.type_only =>
            {
                let source = atom_to_string(&export.src.as_ref().expect("checked").value);
                builder.request_module(&source);
                for (specifier_index, specifier) in export.specifiers.iter().enumerate() {
                    let imported = match specifier {
                        ExportSpecifier::Named(named) if !named.is_type_only => {
                            module_export_name(&named.orig)
                        }
                        ExportSpecifier::Namespace(_) => "*".into(),
                        ExportSpecifier::Default(_) => "default".into(),
                        ExportSpecifier::Named(_) => continue,
                    };
                    builder.declare_import(
                        &reexport_binding_name(item_index, specifier_index),
                        &source,
                        &imported,
                    )?;
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export))
                if matches!(export.decl, swc_ecma_ast::DefaultDecl::TsInterfaceDecl(_)) => {}
            ModuleItem::ModuleDecl(
                ModuleDecl::ExportDefaultExpr(_) | ModuleDecl::ExportDefaultDecl(_),
            ) => {
                builder.declare_local_with_kind(
                    &default_export_binding_name(item_index),
                    BindingKind::Const,
                    false,
                )?;
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportAll(export)) if !export.type_only => {
                let source = atom_to_string(&export.src.value);
                builder.request_module(&source);
                if !builder.star_exports.contains(&source) {
                    builder.star_exports.push(source);
                }
            }
            _ => {}
        }
    }

    for item in &parsed.body {
        match item {
            ModuleItem::Stmt(statement) => {
                builder.initialize_function_declaration(statement)?;
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                if let Decl::Fn(declaration) = &export.decl
                    && !declaration.declare
                {
                    builder.initialize_function_declaration(&Stmt::Decl(Decl::Fn(
                        declaration.clone(),
                    )))?;
                }
            }
            _ => {}
        }
    }

    let mut pending_exports = Vec::<(String, String)>::new();
    for (item_index, item) in parsed.body.iter().enumerate() {
        match item {
            ModuleItem::Stmt(statement) => builder.lower_statement(statement)?,
            ModuleItem::ModuleDecl(ModuleDecl::Import(_)) => {}
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export))
                if is_runtime_erased_declaration(&export.decl) => {}
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::Var(declaration) => {
                    builder.lower_statement(&Stmt::Decl(Decl::Var(declaration.clone())))?;
                    for declarator in &declaration.decls {
                        let mut names = Vec::new();
                        collect_pattern_binding_names(&declarator.name, &mut names)?;
                        pending_exports.extend(names.into_iter().map(|name| (name.clone(), name)));
                    }
                }
                Decl::Fn(declaration) => {
                    let name = declaration.ident.sym.to_string();
                    pending_exports.push((name.clone(), name));
                }
                Decl::Class(declaration) => {
                    builder.lower_statement(&Stmt::Decl(Decl::Class(declaration.clone())))?;
                    let name = declaration.ident.sym.to_string();
                    pending_exports.push((name.clone(), name));
                }
                _ => {
                    return Err(anyhow!(
                        "runtime ESM currently supports exported variable, function and class declarations"
                    ));
                }
            },
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(export)) if export.type_only => {}
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(export)) if !export.type_only => {
                for (specifier_index, specifier) in export.specifiers.iter().enumerate() {
                    let (local, exported) = match specifier {
                        ExportSpecifier::Named(named) if !named.is_type_only => {
                            let local = if export.src.is_some() {
                                reexport_binding_name(item_index, specifier_index)
                            } else {
                                module_export_name(&named.orig)
                            };
                            let exported = named
                                .exported
                                .as_ref()
                                .map(module_export_name)
                                .unwrap_or_else(|| module_export_name(&named.orig));
                            (local, exported)
                        }
                        ExportSpecifier::Namespace(namespace) => (
                            reexport_binding_name(item_index, specifier_index),
                            module_export_name(&namespace.name),
                        ),
                        ExportSpecifier::Default(default) => (
                            reexport_binding_name(item_index, specifier_index),
                            default.exported.sym.to_string(),
                        ),
                        ExportSpecifier::Named(_) => continue,
                    };
                    pending_exports.push((exported, local));
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                let local = default_export_binding_name(item_index);
                let binding = builder
                    .find_binding(&local)
                    .ok_or_else(|| anyhow!("missing synthetic default export binding"))?;
                let value = builder.lower_expression(&export.expr)?;
                builder.emit(Instruction::InitializeBinding { binding, value });
                pending_exports.push(("default".into(), local));
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportAll(_)) => {}
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export))
                if matches!(export.decl, swc_ecma_ast::DefaultDecl::TsInterfaceDecl(_)) => {}
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                let local = default_export_binding_name(item_index);
                let binding = builder
                    .find_binding(&local)
                    .ok_or_else(|| anyhow!("missing synthetic default export binding"))?;
                let value = match &export.decl {
                    swc_ecma_ast::DefaultDecl::Class(class) => {
                        builder.lower_class_expression(class)?
                    }
                    swc_ecma_ast::DefaultDecl::Fn(function) => {
                        let parameters = function
                            .function
                            .params
                            .iter()
                            .map(|parameter| parameter.pat.clone())
                            .collect();
                        let body = function
                            .function
                            .body
                            .as_ref()
                            .ok_or_else(|| anyhow!("runtime default function has no body"))?;
                        builder.lower_nested_function(
                            parameters,
                            &body.stmts,
                            None,
                            function
                                .ident
                                .as_ref()
                                .map(|identifier| identifier.sym.to_string()),
                            function.function.is_async,
                            function.function.is_generator,
                            true,
                            None,
                        )?
                    }
                    swc_ecma_ast::DefaultDecl::TsInterfaceDecl(_) => continue,
                };
                builder.emit(Instruction::InitializeBinding { binding, value });
                pending_exports.push(("default".into(), local));
            }
            ModuleItem::ModuleDecl(_) => {
                return Err(anyhow!("unsupported runtime ESM declaration"));
            }
        }
    }

    for (exported, local_name) in pending_exports {
        let local = builder.find_binding(&local_name).ok_or_else(|| {
            anyhow!("runtime ESM export refers to unknown local binding {local_name:?}")
        })?;
        builder.exports.push(IrExport { exported, local });
    }
    finish_module(builder, specifier, "<module>")
}

fn parse(source: &str, specifier: &str) -> Result<swc_ecma_ast::Module> {
    let source_map: Lrc<SourceMap> = Default::default();
    let file = source_map.new_source_file(
        Lrc::new(FileName::Custom(specifier.to_string())),
        source.to_string(),
    );
    let path = specifier.split(['?', '#']).next().unwrap_or(specifier);
    let syntax = if path.ends_with(".ts")
        || path.ends_with(".tsx")
        || path.ends_with(".mts")
        || path.ends_with(".cts")
    {
        Syntax::Typescript(TsSyntax {
            tsx: path.ends_with(".tsx"),
            decorators: true,
            ..Default::default()
        })
    } else {
        Syntax::Es(EsSyntax {
            jsx: true,
            ..Default::default()
        })
    };
    let mut parser = Parser::new(syntax, StringInput::from(&*file), None);
    parser
        .parse_module()
        .map_err(|error| anyhow!("JavaScript parse error in {specifier}: {error:?}"))
}

fn is_runtime_erased_declaration(declaration: &Decl) -> bool {
    match declaration {
        Decl::Var(declaration) => declaration.declare,
        Decl::Fn(declaration) => declaration.declare,
        Decl::Class(declaration) => declaration.declare,
        Decl::TsInterface(_) | Decl::TsTypeAlias(_) => true,
        Decl::TsEnum(declaration) => declaration.declare,
        Decl::TsModule(declaration) => declaration.declare,
        Decl::Using(_) => false,
    }
}

fn finish_module(mut builder: Builder, specifier: &str, name: &str) -> Result<Module> {
    if !builder.terminated {
        let result = builder.constant(Constant::Undefined);
        builder.terminate(Instruction::Return { value: result });
    }
    builder.seal_current_block();

    let mut functions = vec![Function {
        id: FunctionId(0),
        name: Some(name.into()),
        parameters: Vec::new(),
        rest_parameter: None,
        arguments_binding: None,
        bindings: builder.bindings,
        captures: Vec::new(),
        this_binding: None,
        registers: builder.next_register,
        entry: BlockId(0),
        blocks: builder.blocks,
        exception_regions: builder.exception_regions,
        suspension_points: builder.suspension_points,
        generator_suspension_points: builder.generator_suspension_points,
        is_async: builder.is_async,
        is_generator: builder.is_generator,
        source_span: None,
    }];
    functions.extend(builder.functions);
    let mut module = Module::new(specifier, FunctionId(0), functions);
    module.requested_modules = builder.requested_modules;
    module.star_exports = builder.star_exports;
    module.imports = builder.imports;
    module.exports = builder.exports;
    module.validate().map_err(|error| anyhow!(error))?;
    Ok(module)
}

fn module_export_name(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::Ident(identifier) => identifier.sym.to_string(),
        ModuleExportName::Str(value) => atom_to_string(&value.value),
    }
}

fn atom_to_string(atom: &impl serde::Serialize) -> String {
    let serialized = serde_json::to_value(atom).expect("SWC string atom must serialize");
    serialized
        .as_str()
        .expect("SWC string atom must serialize as a JSON string")
        .to_string()
}

fn reexport_binding_name(item: usize, specifier: usize) -> String {
    format!("*reexport:{item}:{specifier}*")
}

fn default_export_binding_name(item: usize) -> String {
    format!("*default:{item}*")
}

fn module_items_have_use_strict_directive(items: &[ModuleItem]) -> bool {
    let statements = items
        .iter()
        .map_while(|item| match item {
            ModuleItem::Stmt(statement) => Some(statement),
            ModuleItem::ModuleDecl(_) => None,
        })
        .collect::<Vec<_>>();
    statements_have_use_strict_directive(statements)
}

fn statements_have_use_strict_directive<'a>(
    statements: impl IntoIterator<Item = &'a Stmt>,
) -> bool {
    for statement in statements {
        let Stmt::Expr(expression) = statement else {
            break;
        };
        let Expr::Lit(Lit::Str(value)) = expression.expr.as_ref() else {
            break;
        };
        if atom_to_string(&value.value) == "use strict" {
            return true;
        }
    }
    false
}

fn collect_pattern_binding_names(pattern: &Pat, names: &mut Vec<String>) -> Result<()> {
    match pattern {
        Pat::Ident(identifier) => names.push(identifier.id.sym.to_string()),
        Pat::Assign(assignment) => collect_pattern_binding_names(&assignment.left, names)?,
        Pat::Array(array) => {
            for element in array.elems.iter().flatten() {
                if let Pat::Rest(rest) = element {
                    collect_pattern_binding_names(&rest.arg, names)?;
                } else {
                    collect_pattern_binding_names(element, names)?;
                }
            }
        }
        Pat::Object(object) => {
            for property in &object.props {
                match property {
                    swc_ecma_ast::ObjectPatProp::Assign(assign) => {
                        names.push(assign.key.id.sym.to_string());
                    }
                    swc_ecma_ast::ObjectPatProp::KeyValue(key_value) => {
                        collect_pattern_binding_names(&key_value.value, names)?;
                    }
                    swc_ecma_ast::ObjectPatProp::Rest(rest) => {
                        collect_pattern_binding_names(&rest.arg, names)?;
                    }
                }
            }
        }
        _ => {
            return Err(anyhow!(
                "runtime W3IR does not yet support this declaration pattern"
            ));
        }
    }
    Ok(())
}

fn labeled_statement_targets_breakable(statement: &Stmt) -> bool {
    match statement {
        Stmt::While(_)
        | Stmt::DoWhile(_)
        | Stmt::For(_)
        | Stmt::ForIn(_)
        | Stmt::ForOf(_)
        | Stmt::Switch(_) => true,
        Stmt::Labeled(statement) => labeled_statement_targets_breakable(&statement.body),
        _ => false,
    }
}

fn optional_chain_member(expression: &Expr) -> Option<&MemberExpr> {
    match expression {
        Expr::Member(member) => Some(member),
        Expr::OptChain(chain) => match chain.base.as_ref() {
            OptChainBase::Member(member) => Some(member),
            OptChainBase::Call(_) => None,
        },
        Expr::Paren(parenthesized) => optional_chain_member(&parenthesized.expr),
        _ => None,
    }
}

fn object_property_name(name: &PropName) -> Option<String> {
    match name {
        PropName::Ident(identifier) => Some(identifier.sym.to_string()),
        PropName::Str(value) => Some(atom_to_string(&value.value)),
        PropName::Num(value) => Some(value.value.to_string()),
        PropName::BigInt(value) => Some(value.value.to_string()),
        PropName::Computed(_) => None,
    }
}

fn direct_super_call(statement: &Stmt) -> bool {
    matches!(
        statement,
        Stmt::Expr(statement)
            if matches!(
                statement.expr.as_ref(),
                Expr::Call(call) if matches!(call.callee, Callee::Super(_))
            )
    )
}

fn parameter_property_parts(
    property: &swc_ecma_ast::TsParamProp,
) -> Result<(Pat, swc_ecma_ast::Ident)> {
    match &property.param {
        TsParamPropParam::Ident(identifier) => {
            Ok((Pat::Ident(identifier.clone()), identifier.id.clone()))
        }
        TsParamPropParam::Assign(assignment) => {
            let Pat::Ident(identifier) = assignment.left.as_ref() else {
                return Err(anyhow!(
                    "runtime W3IR TypeScript parameter property must bind an identifier"
                ));
            };
            Ok((Pat::Assign(assignment.clone()), identifier.id.clone()))
        }
    }
}

fn parameter_property_assignment(identifier: &swc_ecma_ast::Ident) -> Stmt {
    Stmt::Expr(ExprStmt {
        span: DUMMY_SP,
        expr: Box::new(Expr::Assign(AssignExpr {
            span: DUMMY_SP,
            op: AssignOp::Assign,
            left: AssignTarget::Simple(SimpleAssignTarget::Member(MemberExpr {
                span: DUMMY_SP,
                obj: Box::new(Expr::This(ThisExpr { span: DUMMY_SP })),
                prop: MemberProp::Ident(identifier.clone().into()),
            })),
            right: Box::new(Expr::Ident(identifier.clone())),
        })),
    })
}

#[derive(Clone)]
struct ControlTarget {
    labels: Vec<String>,
    break_block: BlockId,
    continue_block: Option<BlockId>,
    iterator_depth: usize,
    finally_depth: usize,
}

#[derive(Clone)]
struct ActiveIterator {
    iterator: Register,
    is_async: bool,
    protected_blocks: Vec<BlockId>,
}

#[derive(Clone)]
struct ActiveFinally {
    body: BlockStmt,
    /// Exception regions at or above this depth belong to the try/catch being
    /// exited. A cloned finally body must run outside those regions so an
    /// exception from finally overrides the pending completion.
    protection_depth: usize,
}

#[derive(Default)]
struct ActiveProtection {
    blocks: Vec<BlockId>,
}

enum LoweredArguments {
    Registers(Vec<Register>),
    Materialized(Register),
}

#[derive(Clone, Copy)]
enum LoweredAssignmentTarget {
    Binding {
        binding: BindingId,
        mutable: bool,
    },
    Property {
        object: Register,
        key: Register,
    },
    Private {
        object: Register,
        brand: Register,
        name: Register,
    },
    Super {
        parent: Register,
        receiver: Register,
        key: Register,
        is_static: bool,
    },
}

struct Builder {
    is_entry: bool,
    instructions: Vec<Instruction>,
    blocks: Vec<Block>,
    current_block: BlockId,
    next_block: u32,
    control_targets: Vec<ControlTarget>,
    pending_control_labels: Vec<String>,
    active_iterators: Vec<ActiveIterator>,
    active_finalizers: Vec<ActiveFinally>,
    active_protections: Vec<ActiveProtection>,
    exception_regions: Vec<ExceptionRegion>,
    bindings: Vec<Binding>,
    parameters: Vec<BindingId>,
    rest_parameter: Option<BindingId>,
    arguments_binding: Option<BindingId>,
    requested_modules: Vec<String>,
    star_exports: Vec<String>,
    imports: Vec<Import>,
    exports: Vec<IrExport>,
    globals: HashMap<String, BindingId>,
    scopes: Vec<HashMap<String, BindingId>>,
    outer: HashMap<String, BindingId>,
    captures: HashMap<String, BindingId>,
    capture_order: Vec<BindingId>,
    new_globals: Vec<Binding>,
    functions: Vec<Function>,
    next_register: u32,
    next_binding: u32,
    next_function: u32,
    next_suspension: u32,
    allows_await: bool,
    is_async: bool,
    is_generator: bool,
    /// `Some(false)` for instance class members, `Some(true)` for static
    /// members, and `None` outside class-member lexical `super` scope.
    class_super_is_static: Option<bool>,
    annex_b_function_declarations: bool,
    suspension_points: Vec<SuspensionPoint>,
    generator_suspension_points: Vec<GeneratorSuspensionPoint>,
    terminated: bool,
}

impl Builder {
    fn entry(allows_await: bool, annex_b_function_declarations: bool) -> Self {
        Self {
            is_entry: true,
            instructions: Vec::new(),
            blocks: Vec::new(),
            current_block: BlockId(0),
            next_block: 1,
            control_targets: Vec::new(),
            pending_control_labels: Vec::new(),
            active_iterators: Vec::new(),
            active_finalizers: Vec::new(),
            active_protections: Vec::new(),
            exception_regions: Vec::new(),
            bindings: Vec::new(),
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            requested_modules: Vec::new(),
            star_exports: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            globals: HashMap::new(),
            scopes: vec![HashMap::new()],
            outer: HashMap::new(),
            captures: HashMap::new(),
            capture_order: Vec::new(),
            new_globals: Vec::new(),
            functions: Vec::new(),
            next_register: 0,
            next_binding: 0,
            next_function: 1,
            next_suspension: 0,
            allows_await,
            is_async: false,
            is_generator: false,
            class_super_is_static: None,
            annex_b_function_declarations,
            suspension_points: Vec::new(),
            generator_suspension_points: Vec::new(),
            terminated: false,
        }
    }

    fn nested(
        outer: HashMap<String, BindingId>,
        globals: HashMap<String, BindingId>,
        next_binding: u32,
        next_function: u32,
        is_async: bool,
        is_generator: bool,
        annex_b_function_declarations: bool,
    ) -> Self {
        Self {
            is_entry: false,
            instructions: Vec::new(),
            blocks: Vec::new(),
            current_block: BlockId(0),
            next_block: 1,
            control_targets: Vec::new(),
            pending_control_labels: Vec::new(),
            active_iterators: Vec::new(),
            active_finalizers: Vec::new(),
            active_protections: Vec::new(),
            exception_regions: Vec::new(),
            bindings: Vec::new(),
            parameters: Vec::new(),
            rest_parameter: None,
            arguments_binding: None,
            requested_modules: Vec::new(),
            star_exports: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            globals,
            scopes: vec![HashMap::new()],
            outer,
            captures: HashMap::new(),
            capture_order: Vec::new(),
            new_globals: Vec::new(),
            functions: Vec::new(),
            next_register: 0,
            next_binding,
            next_function,
            next_suspension: 0,
            allows_await: is_async,
            is_async,
            is_generator,
            class_super_is_static: None,
            annex_b_function_declarations,
            suspension_points: Vec::new(),
            generator_suspension_points: Vec::new(),
            terminated: false,
        }
    }

    fn register(&mut self) -> Register {
        let register = Register(self.next_register);
        self.next_register += 1;
        register
    }

    fn emit(&mut self, instruction: Instruction) {
        debug_assert!(!self.terminated);
        for context in &mut self.active_iterators {
            if !context.protected_blocks.contains(&self.current_block) {
                context.protected_blocks.push(self.current_block);
            }
        }
        for context in &mut self.active_protections {
            if !context.blocks.contains(&self.current_block) {
                context.blocks.push(self.current_block);
            }
        }
        self.instructions.push(instruction);
    }

    fn lower_arguments(
        &mut self,
        arguments: &[ExprOrSpread],
        prefix: &[Register],
    ) -> Result<LoweredArguments> {
        if arguments.iter().any(|argument| argument.spread.is_some()) {
            let materialized = self.register();
            self.emit(Instruction::CreateArray {
                dst: materialized,
                elements: Vec::new(),
            });
            for value in prefix {
                self.emit(Instruction::AppendArrayElement {
                    array: materialized,
                    value: *value,
                });
            }
            for argument in arguments {
                let value = self.lower_expression(&argument.expr)?;
                if argument.spread.is_some() {
                    self.emit(Instruction::AppendIterable {
                        array: materialized,
                        iterable: value,
                    });
                } else {
                    self.emit(Instruction::AppendArrayElement {
                        array: materialized,
                        value,
                    });
                }
            }
            Ok(LoweredArguments::Materialized(materialized))
        } else {
            let mut lowered = Vec::with_capacity(prefix.len() + arguments.len());
            lowered.extend_from_slice(prefix);
            for argument in arguments {
                lowered.push(self.lower_expression(&argument.expr)?);
            }
            Ok(LoweredArguments::Registers(lowered))
        }
    }

    fn append_array_hole(&mut self, array: Register) {
        let undefined = self.constant(Constant::Undefined);
        self.emit(Instruction::AppendArrayElement {
            array,
            value: undefined,
        });
        let length_key = self.constant(Constant::String("length".into()));
        let length = self.register();
        self.emit(Instruction::GetProperty {
            dst: length,
            object: array,
            key: length_key,
        });
        let one = self.constant(Constant::Number(1.0));
        let index = self.register();
        self.emit(Instruction::Binary {
            dst: index,
            operator: BinaryOperator::Subtract,
            lhs: length,
            rhs: one,
        });
        let deleted = self.register();
        self.emit(Instruction::DeleteProperty {
            dst: deleted,
            object: array,
            key: index,
        });
    }

    fn emit_call(
        &mut self,
        dst: Register,
        callee: Register,
        this_value: Register,
        arguments: LoweredArguments,
    ) {
        match arguments {
            LoweredArguments::Registers(arguments) => self.emit(Instruction::Call {
                dst,
                callee,
                this_value,
                arguments,
            }),
            LoweredArguments::Materialized(arguments) => {
                self.emit(Instruction::CallWithArguments {
                    dst,
                    callee,
                    this_value,
                    arguments,
                });
            }
        }
    }

    fn emit_method_call(
        &mut self,
        dst: Register,
        object: Register,
        key: Register,
        arguments: LoweredArguments,
    ) {
        match arguments {
            LoweredArguments::Registers(arguments) => self.emit(Instruction::CallMethod {
                dst,
                object,
                key,
                arguments,
            }),
            LoweredArguments::Materialized(arguments) => {
                self.emit(Instruction::CallMethodWithArguments {
                    dst,
                    object,
                    key,
                    arguments,
                });
            }
        }
    }

    fn emit_construct(
        &mut self,
        dst: Register,
        constructor: Register,
        arguments: LoweredArguments,
    ) {
        match arguments {
            LoweredArguments::Registers(arguments) => self.emit(Instruction::Construct {
                dst,
                constructor,
                arguments,
            }),
            LoweredArguments::Materialized(arguments) => {
                self.emit(Instruction::ConstructWithArguments {
                    dst,
                    constructor,
                    arguments,
                });
            }
        }
    }

    fn terminate(&mut self, instruction: Instruction) {
        self.emit(instruction);
        self.terminated = true;
    }

    fn allocate_block(&mut self) -> BlockId {
        let block = BlockId(self.next_block);
        self.next_block += 1;
        block
    }

    fn seal_current_block(&mut self) {
        self.blocks.push(Block {
            id: self.current_block,
            instructions: std::mem::take(&mut self.instructions),
            source_span: None,
        });
    }

    fn start_block(&mut self, block: BlockId) {
        self.current_block = block;
        self.terminated = false;
    }

    fn constant(&mut self, value: Constant) -> Register {
        let dst = self.register();
        self.emit(Instruction::LoadConstant { dst, value });
        dst
    }

    fn global(&mut self, name: &str) -> BindingId {
        if let Some(binding) = self.globals.get(name) {
            let binding = *binding;
            if !self.is_entry {
                self.capture(name, binding);
            }
            return binding;
        }
        let binding = BindingId(self.next_binding);
        self.next_binding += 1;
        self.globals.insert(name.to_string(), binding);
        let declaration = Binding {
            id: binding,
            name: name.to_string(),
            kind: BindingKind::Import,
            mutable: false,
        };
        if self.is_entry {
            self.bindings.push(declaration);
        } else {
            self.new_globals.push(declaration);
            self.capture(name, binding);
        }
        self.imports.push(Import {
            specifier: "w3cos:global".into(),
            imported: name.to_string(),
            local: binding,
        });
        binding
    }

    fn capture(&mut self, name: &str, binding: BindingId) {
        if self.captures.insert(name.to_string(), binding).is_none() {
            self.capture_order.push(binding);
        }
    }

    fn visible_bindings(&self) -> HashMap<String, BindingId> {
        let mut visible = self.outer.clone();
        visible.extend(self.globals.iter().map(|(name, id)| (name.clone(), *id)));
        visible.extend(self.captures.iter().map(|(name, id)| (name.clone(), *id)));
        for scope in &self.scopes {
            visible.extend(scope.iter().map(|(name, id)| (name.clone(), *id)));
        }
        visible
    }

    fn request_module(&mut self, specifier: &str) {
        if !self
            .requested_modules
            .iter()
            .any(|requested| requested == specifier)
        {
            self.requested_modules.push(specifier.to_string());
        }
    }

    fn declare_import(
        &mut self,
        local_name: &str,
        specifier: &str,
        imported: &str,
    ) -> Result<BindingId> {
        let binding = self.declare_local_with_kind(local_name, BindingKind::Import, false)?;
        self.imports.push(Import {
            specifier: specifier.to_string(),
            imported: imported.to_string(),
            local: binding,
        });
        Ok(binding)
    }

    fn declare_external(&mut self, name: &str, mutable: bool) -> Result<BindingId> {
        let binding = self.declare_local_with_kind(name, BindingKind::Import, mutable)?;
        self.imports.push(Import {
            specifier: "w3cos:external".into(),
            imported: name.to_string(),
            local: binding,
        });
        Ok(binding)
    }

    fn find_binding(&self, name: &str) -> Option<BindingId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .or_else(|| self.captures.get(name).copied())
            .or_else(|| self.outer.get(name).copied())
    }

    fn declare_local(&mut self, name: &str, kind: VarDeclKind) -> Result<BindingId> {
        let scope_index = if kind == VarDeclKind::Var {
            0
        } else {
            self.scopes.len() - 1
        };
        if let Some(binding) = self.scopes[scope_index].get(name) {
            if kind == VarDeclKind::Var {
                return Ok(*binding);
            }
            return Err(anyhow!(
                "duplicate lexical binding {name:?} in the same block"
            ));
        }
        let binding = BindingId(self.next_binding);
        self.next_binding += 1;
        self.scopes[scope_index].insert(name.to_string(), binding);
        self.bindings.push(Binding {
            id: binding,
            name: name.to_string(),
            kind: match kind {
                VarDeclKind::Var => BindingKind::Var,
                VarDeclKind::Let => BindingKind::Let,
                VarDeclKind::Const => BindingKind::Const,
            },
            mutable: kind != VarDeclKind::Const,
        });
        Ok(binding)
    }

    fn predeclare_var_bindings(&mut self, statement: &Stmt) -> Result<()> {
        match statement {
            Stmt::Decl(Decl::Var(declaration))
                if declaration.kind == VarDeclKind::Var && !declaration.declare =>
            {
                for declarator in &declaration.decls {
                    self.declare_variable_pattern_bindings(
                        &declarator.name,
                        VarDeclKind::Var,
                        &mut Vec::new(),
                    )?;
                }
            }
            Stmt::Block(block) => {
                for statement in &block.stmts {
                    self.predeclare_var_bindings(statement)?;
                }
            }
            Stmt::If(statement) => {
                self.predeclare_var_bindings(&statement.cons)?;
                if let Some(alternate) = &statement.alt {
                    self.predeclare_var_bindings(alternate)?;
                }
            }
            Stmt::While(statement) => self.predeclare_var_bindings(&statement.body)?,
            Stmt::DoWhile(statement) => self.predeclare_var_bindings(&statement.body)?,
            Stmt::For(statement) => {
                if let Some(swc_ecma_ast::VarDeclOrExpr::VarDecl(declaration)) = &statement.init
                    && declaration.kind == VarDeclKind::Var
                {
                    for declarator in &declaration.decls {
                        self.declare_variable_pattern_bindings(
                            &declarator.name,
                            VarDeclKind::Var,
                            &mut Vec::new(),
                        )?;
                    }
                }
                self.predeclare_var_bindings(&statement.body)?;
            }
            Stmt::ForOf(statement) => {
                if let ForHead::VarDecl(declaration) = &statement.left
                    && declaration.kind == VarDeclKind::Var
                {
                    for declarator in &declaration.decls {
                        self.declare_variable_pattern_bindings(
                            &declarator.name,
                            VarDeclKind::Var,
                            &mut Vec::new(),
                        )?;
                    }
                }
                self.predeclare_var_bindings(&statement.body)?;
            }
            Stmt::ForIn(statement) => {
                if let ForHead::VarDecl(declaration) = &statement.left
                    && declaration.kind == VarDeclKind::Var
                {
                    for declarator in &declaration.decls {
                        self.declare_variable_pattern_bindings(
                            &declarator.name,
                            VarDeclKind::Var,
                            &mut Vec::new(),
                        )?;
                    }
                }
                self.predeclare_var_bindings(&statement.body)?;
            }
            Stmt::Switch(statement) => {
                for case in &statement.cases {
                    for statement in &case.cons {
                        self.predeclare_var_bindings(statement)?;
                    }
                }
            }
            Stmt::Try(statement) => {
                for statement in &statement.block.stmts {
                    self.predeclare_var_bindings(statement)?;
                }
                if let Some(handler) = &statement.handler {
                    for statement in &handler.body.stmts {
                        self.predeclare_var_bindings(statement)?;
                    }
                }
                if let Some(finalizer) = &statement.finalizer {
                    for statement in &finalizer.stmts {
                        self.predeclare_var_bindings(statement)?;
                    }
                }
            }
            Stmt::Labeled(statement) => self.predeclare_var_bindings(&statement.body)?,
            _ => {}
        }
        Ok(())
    }

    fn predeclare_function_binding(&mut self, statement: &Stmt) -> Result<()> {
        let Stmt::Decl(Decl::Fn(declaration)) = statement else {
            return Ok(());
        };
        if declaration.declare {
            return Ok(());
        }
        self.declare_local(declaration.ident.sym.as_ref(), VarDeclKind::Var)?;
        Ok(())
    }

    fn predeclare_annex_b_branch_functions(&mut self, statement: &Stmt) -> Result<()> {
        if !self.annex_b_function_declarations {
            return Ok(());
        }
        match statement {
            Stmt::If(statement) => {
                self.predeclare_annex_b_branch_function(&statement.cons)?;
                self.predeclare_annex_b_branch_functions(&statement.cons)?;
                if let Some(alternate) = &statement.alt {
                    self.predeclare_annex_b_branch_function(alternate)?;
                    self.predeclare_annex_b_branch_functions(alternate)?;
                }
            }
            Stmt::Block(block) => {
                for statement in &block.stmts {
                    self.predeclare_annex_b_branch_functions(statement)?;
                }
            }
            Stmt::While(statement) => self.predeclare_annex_b_branch_functions(&statement.body)?,
            Stmt::DoWhile(statement) => {
                self.predeclare_annex_b_branch_functions(&statement.body)?
            }
            Stmt::For(statement) => self.predeclare_annex_b_branch_functions(&statement.body)?,
            Stmt::ForIn(statement) => self.predeclare_annex_b_branch_functions(&statement.body)?,
            Stmt::ForOf(statement) => self.predeclare_annex_b_branch_functions(&statement.body)?,
            Stmt::Switch(statement) => {
                for case in &statement.cases {
                    for statement in &case.cons {
                        self.predeclare_annex_b_branch_functions(statement)?;
                    }
                }
            }
            Stmt::Labeled(statement) => {
                self.predeclare_annex_b_branch_functions(&statement.body)?
            }
            _ => {}
        }
        Ok(())
    }

    fn predeclare_annex_b_branch_function(&mut self, statement: &Stmt) -> Result<()> {
        let Stmt::Decl(Decl::Fn(declaration)) = statement else {
            return Ok(());
        };
        if declaration.declare {
            return Ok(());
        }
        self.declare_local(declaration.ident.sym.as_ref(), VarDeclKind::Var)?;
        Ok(())
    }

    fn predeclare_block_function_binding(&mut self, statement: &Stmt) -> Result<()> {
        let Stmt::Decl(Decl::Fn(declaration)) = statement else {
            return Ok(());
        };
        if declaration.declare {
            return Ok(());
        }
        self.declare_local(declaration.ident.sym.as_ref(), VarDeclKind::Let)?;
        Ok(())
    }

    fn initialize_function_declaration(&mut self, statement: &Stmt) -> Result<()> {
        let Stmt::Decl(Decl::Fn(declaration)) = statement else {
            return Ok(());
        };
        if declaration.declare {
            return Ok(());
        }
        let name = declaration.ident.sym.to_string();
        let binding = self
            .find_binding(&name)
            .ok_or_else(|| anyhow!("missing predeclared function binding {name:?}"))?;
        let body = declaration
            .function
            .body
            .as_ref()
            .ok_or_else(|| anyhow!("runtime function declaration has no body"))?;
        let parameters = declaration
            .function
            .params
            .iter()
            .map(|parameter| parameter.pat.clone())
            .collect();
        let value = self.lower_nested_function(
            parameters,
            &body.stmts,
            None,
            Some(name.clone()),
            declaration.function.is_async,
            declaration.function.is_generator,
            true,
            None,
        )?;
        let kind = self
            .bindings
            .iter()
            .find(|candidate| candidate.id == binding)
            .map(|binding| binding.kind)
            .ok_or_else(|| anyhow!("missing function declaration binding {name:?}"))?;
        if matches!(kind, BindingKind::Let | BindingKind::Const) {
            self.emit(Instruction::InitializeBinding { binding, value });
        } else {
            self.emit(Instruction::StoreBinding { binding, value });
        }
        Ok(())
    }

    fn predeclare_lexical_binding(&mut self, statement: &Stmt) -> Result<()> {
        match statement {
            Stmt::Decl(Decl::Var(declaration))
                if declaration.kind != VarDeclKind::Var && !declaration.declare =>
            {
                for declarator in &declaration.decls {
                    self.declare_variable_pattern_bindings(
                        &declarator.name,
                        declaration.kind,
                        &mut Vec::new(),
                    )?;
                }
            }
            Stmt::Decl(Decl::Class(declaration)) if !declaration.declare => {
                self.declare_class_binding(declaration.ident.sym.as_ref())?;
            }
            _ => {}
        }
        Ok(())
    }

    fn declare_class_binding(&mut self, name: &str) -> Result<BindingId> {
        self.declare_current_binding(name, BindingKind::Class, false)
    }

    fn declare_current_binding(
        &mut self,
        name: &str,
        kind: BindingKind,
        mutable: bool,
    ) -> Result<BindingId> {
        let scope = self.scopes.last_mut().expect("builder always has a scope");
        if scope.contains_key(name) {
            return Err(anyhow!(
                "duplicate lexical binding {name:?} in the same block"
            ));
        }
        let binding = BindingId(self.next_binding);
        self.next_binding += 1;
        scope.insert(name.to_string(), binding);
        self.bindings.push(Binding {
            id: binding,
            name: name.to_string(),
            kind,
            mutable,
        });
        Ok(binding)
    }

    fn declare_variable_pattern_bindings(
        &mut self,
        pattern: &Pat,
        kind: VarDeclKind,
        declared: &mut Vec<BindingId>,
    ) -> Result<()> {
        match pattern {
            Pat::Ident(identifier) => {
                declared.push(self.declare_local(identifier.id.sym.as_ref(), kind)?);
            }
            Pat::Assign(assignment) => {
                self.declare_variable_pattern_bindings(&assignment.left, kind, declared)?;
            }
            Pat::Array(array) => {
                for element in array.elems.iter().flatten() {
                    if let Pat::Rest(rest) = element {
                        self.declare_variable_pattern_bindings(&rest.arg, kind, declared)?;
                    } else {
                        self.declare_variable_pattern_bindings(element, kind, declared)?;
                    }
                }
            }
            Pat::Object(object) => {
                for property in &object.props {
                    match property {
                        swc_ecma_ast::ObjectPatProp::Assign(assign) => {
                            declared.push(self.declare_local(assign.key.id.sym.as_ref(), kind)?);
                        }
                        swc_ecma_ast::ObjectPatProp::KeyValue(key_value) => {
                            self.declare_variable_pattern_bindings(
                                &key_value.value,
                                kind,
                                declared,
                            )?;
                        }
                        swc_ecma_ast::ObjectPatProp::Rest(rest) => {
                            self.declare_variable_pattern_bindings(&rest.arg, kind, declared)?;
                        }
                    }
                }
            }
            _ => {
                return Err(anyhow!(
                    "runtime W3IR does not yet support this variable declaration pattern"
                ));
            }
        }
        Ok(())
    }

    fn load_binding(&mut self, binding: BindingId) -> Register {
        let dst = self.register();
        self.emit(Instruction::LoadBinding { dst, binding });
        dst
    }

    fn resolve_binding(&mut self, name: &str) -> BindingId {
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.get(name) {
                return *binding;
            }
        }
        if let Some(binding) = self.captures.get(name) {
            return *binding;
        }
        if let Some(binding) = self.outer.get(name) {
            let binding = *binding;
            self.capture(name, binding);
            return binding;
        }
        self.global(name)
    }

    fn declare_parameter(&mut self, pattern: &Pat, index: usize) -> Result<Option<BindingId>> {
        let source = self.declare_hidden_parameter(index);
        if let Pat::Rest(rest) = pattern {
            let Pat::Ident(identifier) = rest.arg.as_ref() else {
                return Err(anyhow!(
                    "runtime W3IR rest parameters currently require identifier patterns"
                ));
            };
            if self.rest_parameter.is_some() {
                return Err(anyhow!(
                    "runtime W3IR function has multiple rest parameters"
                ));
            }
            self.declare_local_with_kind(identifier.id.sym.as_ref(), BindingKind::Let, true)?;
            self.rest_parameter = Some(source);
            return Ok(Some(source));
        }

        match pattern {
            Pat::Ident(identifier) => {
                self.declare_local_with_kind(identifier.id.sym.as_ref(), BindingKind::Let, true)?;
            }
            Pat::Assign(assignment) if matches!(assignment.left.as_ref(), Pat::Ident(_)) => {
                let Pat::Ident(identifier) = assignment.left.as_ref() else {
                    unreachable!()
                };
                self.declare_local_with_kind(identifier.id.sym.as_ref(), BindingKind::Let, true)?;
            }
            Pat::Assign(assignment) => {
                self.declare_parameter_pattern_bindings(&assignment.left)?;
            }
            Pat::Array(_) | Pat::Object(_) => {
                self.declare_parameter_pattern_bindings(pattern)?;
            }
            _ => {
                return Err(anyhow!(
                    "runtime W3IR does not yet support this parameter pattern"
                ));
            }
        }
        self.parameters.push(source);
        Ok(Some(source))
    }

    fn declare_hidden_parameter(&mut self, index: usize) -> BindingId {
        let binding = BindingId(self.next_binding);
        self.next_binding += 1;
        self.bindings.push(Binding {
            id: binding,
            name: format!("*parameter:{index}*"),
            kind: BindingKind::Parameter,
            mutable: true,
        });
        binding
    }

    fn declare_parameter_pattern_bindings(&mut self, pattern: &Pat) -> Result<()> {
        match pattern {
            Pat::Ident(identifier) => {
                self.declare_local_with_kind(identifier.id.sym.as_ref(), BindingKind::Let, true)?;
            }
            Pat::Assign(assignment) => {
                self.declare_parameter_pattern_bindings(&assignment.left)?;
            }
            Pat::Array(array) => {
                for element in array.elems.iter().flatten() {
                    if let Pat::Rest(rest) = element {
                        self.declare_parameter_pattern_bindings(&rest.arg)?;
                    } else {
                        self.declare_parameter_pattern_bindings(element)?;
                    }
                }
            }
            Pat::Object(object) => {
                for property in &object.props {
                    match property {
                        swc_ecma_ast::ObjectPatProp::Assign(assign) => {
                            self.declare_local_with_kind(
                                assign.key.id.sym.as_ref(),
                                BindingKind::Let,
                                true,
                            )?;
                        }
                        swc_ecma_ast::ObjectPatProp::KeyValue(key_value) => {
                            self.declare_parameter_pattern_bindings(&key_value.value)?;
                        }
                        swc_ecma_ast::ObjectPatProp::Rest(rest) => {
                            self.declare_parameter_pattern_bindings(&rest.arg)?;
                        }
                    }
                }
            }
            _ => {
                return Err(anyhow!(
                    "runtime W3IR does not yet support this nested parameter pattern"
                ));
            }
        }
        Ok(())
    }

    fn declare_catch_pattern_bindings(&mut self, pattern: &Pat) -> Result<()> {
        match pattern {
            Pat::Ident(identifier) => {
                self.declare_current_binding(identifier.id.sym.as_ref(), BindingKind::Catch, true)?;
            }
            Pat::Assign(assignment) => {
                self.declare_catch_pattern_bindings(&assignment.left)?;
            }
            Pat::Array(array) => {
                for element in array.elems.iter().flatten() {
                    if let Pat::Rest(rest) = element {
                        self.declare_catch_pattern_bindings(&rest.arg)?;
                    } else {
                        self.declare_catch_pattern_bindings(element)?;
                    }
                }
            }
            Pat::Object(object) => {
                for property in &object.props {
                    match property {
                        swc_ecma_ast::ObjectPatProp::Assign(assign) => {
                            self.declare_current_binding(
                                assign.key.id.sym.as_ref(),
                                BindingKind::Catch,
                                true,
                            )?;
                        }
                        swc_ecma_ast::ObjectPatProp::KeyValue(key_value) => {
                            self.declare_catch_pattern_bindings(&key_value.value)?;
                        }
                        swc_ecma_ast::ObjectPatProp::Rest(rest) => {
                            self.declare_catch_pattern_bindings(&rest.arg)?;
                        }
                    }
                }
            }
            Pat::Rest(rest) => {
                self.declare_catch_pattern_bindings(&rest.arg)?;
            }
            _ => {
                return Err(anyhow!(
                    "runtime W3IR does not yet support this catch binding pattern"
                ));
            }
        }
        Ok(())
    }

    fn initialize_parameter(&mut self, pattern: &Pat, source: Option<BindingId>) -> Result<()> {
        let source =
            source.ok_or_else(|| anyhow!("missing runtime W3IR parameter source binding"))?;
        let value = self.load_binding(source);
        if let Pat::Rest(rest) = pattern {
            self.initialize_binding_pattern(&rest.arg, value)
        } else {
            self.initialize_binding_pattern(pattern, value)
        }
    }

    fn initialize_binding_pattern(&mut self, pattern: &Pat, value: Register) -> Result<()> {
        self.write_binding_pattern(pattern, value, false)
    }

    fn assign_binding_pattern(&mut self, pattern: &Pat, value: Register) -> Result<()> {
        self.write_binding_pattern(pattern, value, true)
    }

    fn write_binding_pattern(
        &mut self,
        pattern: &Pat,
        value: Register,
        assignment: bool,
    ) -> Result<()> {
        match pattern {
            Pat::Ident(identifier) => {
                let name = identifier.id.sym.to_string();
                let binding = self
                    .find_binding(&name)
                    .ok_or_else(|| anyhow!("missing runtime pattern binding {name:?}"))?;
                if assignment {
                    self.ensure_mutable_binding(&name, binding)?;
                    self.emit(Instruction::StoreBinding { binding, value });
                    return Ok(());
                }
                let kind = self
                    .bindings
                    .iter()
                    .find(|candidate| candidate.id == binding)
                    .map(|binding| binding.kind)
                    .ok_or_else(|| anyhow!("missing runtime pattern declaration {name:?}"))?;
                if matches!(
                    kind,
                    BindingKind::Let | BindingKind::Const | BindingKind::Catch
                ) {
                    self.emit(Instruction::InitializeBinding { binding, value });
                } else {
                    self.emit(Instruction::StoreBinding { binding, value });
                }
                Ok(())
            }
            Pat::Assign(default_pattern) => self.write_binding_default(
                &default_pattern.left,
                value,
                &default_pattern.right,
                assignment,
            ),
            Pat::Array(array) => {
                for (index, element) in array.elems.iter().enumerate() {
                    let Some(element) = element else {
                        continue;
                    };
                    if let Pat::Rest(rest) = element {
                        let rest_value = self.register();
                        self.emit(Instruction::ArrayRest {
                            dst: rest_value,
                            value,
                            start: index as u32,
                        });
                        self.write_binding_pattern(&rest.arg, rest_value, assignment)?;
                        continue;
                    }
                    let key = self.constant(Constant::Number(index as f64));
                    let element_value = self.register();
                    self.emit(Instruction::GetProperty {
                        dst: element_value,
                        object: value,
                        key,
                    });
                    self.write_binding_pattern(element, element_value, assignment)?;
                }
                Ok(())
            }
            Pat::Object(object) => {
                let mut excluded = Vec::new();
                for property in &object.props {
                    match property {
                        swc_ecma_ast::ObjectPatProp::Assign(assign) => {
                            let key =
                                self.constant(Constant::String(assign.key.id.sym.to_string()));
                            excluded.push(key);
                            let property_value = self.register();
                            self.emit(Instruction::GetProperty {
                                dst: property_value,
                                object: value,
                                key,
                            });
                            let target = Pat::Ident(assign.key.clone());
                            if let Some(default) = &assign.value {
                                self.write_binding_default(
                                    &target,
                                    property_value,
                                    default,
                                    assignment,
                                )?;
                            } else {
                                self.write_binding_pattern(&target, property_value, assignment)?;
                            }
                        }
                        swc_ecma_ast::ObjectPatProp::KeyValue(key_value) => {
                            let key = self.lower_property_name(&key_value.key)?;
                            excluded.push(key);
                            let property_value = self.register();
                            self.emit(Instruction::GetProperty {
                                dst: property_value,
                                object: value,
                                key,
                            });
                            self.write_binding_pattern(
                                &key_value.value,
                                property_value,
                                assignment,
                            )?;
                        }
                        swc_ecma_ast::ObjectPatProp::Rest(rest) => {
                            let rest_value = self.register();
                            self.emit(Instruction::ObjectRest {
                                dst: rest_value,
                                value,
                                excluded: excluded.clone(),
                            });
                            self.write_binding_pattern(&rest.arg, rest_value, assignment)?;
                        }
                    }
                }
                Ok(())
            }
            Pat::Expr(expression) if assignment => {
                let Expr::Member(member) = expression.as_ref() else {
                    return Err(anyhow!(
                        "runtime W3IR assignment patterns require identifier or member targets"
                    ));
                };
                let target = if matches!(member.prop, MemberProp::PrivateName(_)) {
                    let (object, brand, name) = self.lower_private_member_parts(member)?;
                    LoweredAssignmentTarget::Private {
                        object,
                        brand,
                        name,
                    }
                } else {
                    let (object, key) = self.lower_member_parts(member)?;
                    LoweredAssignmentTarget::Property { object, key }
                };
                self.store_assignment_target(target, value);
                Ok(())
            }
            _ => Err(anyhow!(
                "runtime W3IR does not yet support this binding initialization pattern"
            )),
        }
    }

    fn write_binding_default(
        &mut self,
        pattern: &Pat,
        current: Register,
        default: &Expr,
        assignment: bool,
    ) -> Result<()> {
        let undefined = self.constant(Constant::Undefined);
        let condition = self.register();
        self.emit(Instruction::Binary {
            dst: condition,
            operator: BinaryOperator::StrictEqual,
            lhs: current,
            rhs: undefined,
        });
        let initialize_block = self.allocate_block();
        let preserve_block = self.allocate_block();
        let merge_block = self.allocate_block();
        self.terminate(Instruction::Branch {
            condition,
            then_block: initialize_block,
            else_block: preserve_block,
        });
        self.seal_current_block();

        self.start_block(initialize_block);
        let value = self.lower_expression(default)?;
        self.write_binding_pattern(pattern, value, assignment)?;
        self.terminate(Instruction::Jump {
            target: merge_block,
        });
        self.seal_current_block();

        self.start_block(preserve_block);
        self.write_binding_pattern(pattern, current, assignment)?;
        self.terminate(Instruction::Jump {
            target: merge_block,
        });
        self.seal_current_block();
        self.start_block(merge_block);
        Ok(())
    }

    fn declare_local_with_kind(
        &mut self,
        name: &str,
        kind: BindingKind,
        mutable: bool,
    ) -> Result<BindingId> {
        let function_scope = &mut self.scopes[0];
        if function_scope.contains_key(name) {
            return Err(anyhow!(
                "duplicate binding {name:?} is not yet supported in runtime W3IR"
            ));
        }
        let binding = BindingId(self.next_binding);
        self.next_binding += 1;
        function_scope.insert(name.to_string(), binding);
        self.bindings.push(Binding {
            id: binding,
            name: name.to_string(),
            kind,
            mutable,
        });
        Ok(binding)
    }

    fn declare_this_binding(&mut self) -> Result<BindingId> {
        self.declare_local_with_kind("*this*", BindingKind::Parameter, false)
    }

    fn declare_arguments_binding(&mut self) -> Result<()> {
        if self.scopes[0].contains_key("arguments") {
            return Ok(());
        }
        let binding = self.declare_local_with_kind("arguments", BindingKind::Parameter, false)?;
        self.arguments_binding = Some(binding);
        Ok(())
    }

    fn lower_statement(&mut self, statement: &Stmt) -> Result<()> {
        if self.terminated {
            return Ok(());
        }
        match statement {
            Stmt::Expr(expression) => {
                self.lower_expression(&expression.expr)?;
                Ok(())
            }
            Stmt::Empty(_) => Ok(()),
            Stmt::Debugger(_) => Ok(()),
            Stmt::Decl(declaration) if is_runtime_erased_declaration(declaration) => Ok(()),
            Stmt::Block(block) => {
                self.scopes.push(HashMap::new());
                let result = (|| {
                    for statement in &block.stmts {
                        self.predeclare_lexical_binding(statement)?;
                        self.predeclare_block_function_binding(statement)?;
                    }
                    let mut block_bindings = self
                        .scopes
                        .last()
                        .unwrap()
                        .values()
                        .copied()
                        .collect::<Vec<_>>();
                    block_bindings.sort_by_key(|binding| binding.0);
                    for binding in block_bindings {
                        self.emit(Instruction::RefreshBinding { binding });
                    }
                    for statement in &block.stmts {
                        self.initialize_function_declaration(statement)?;
                    }
                    for statement in &block.stmts {
                        self.lower_statement(statement)?;
                    }
                    Ok(())
                })();
                self.scopes.pop();
                result
            }
            Stmt::Decl(Decl::Var(declaration)) => {
                for declarator in &declaration.decls {
                    match &declarator.init {
                        Some(initializer) => {
                            // Preserve the surrounding binding name for
                            // anonymous function expressions. Besides making
                            // W3IR diagnostics useful, this gives every
                            // backend a stable identity for `const values =
                            // function* () { ... }` without teaching the AOT
                            // path to rediscover functions from source spans.
                            let value = match (&declarator.name, initializer.as_ref()) {
                                (Pat::Ident(binding), Expr::Fn(function)) => {
                                    let parameters = function
                                        .function
                                        .params
                                        .iter()
                                        .map(|parameter| parameter.pat.clone())
                                        .collect();
                                    let body =
                                        function.function.body.as_ref().ok_or_else(|| {
                                            anyhow!("runtime function expression has no body")
                                        })?;
                                    self.lower_nested_function(
                                        parameters,
                                        &body.stmts,
                                        None,
                                        Some(binding.id.sym.to_string()),
                                        function.function.is_async,
                                        function.function.is_generator,
                                        true,
                                        None,
                                    )?
                                }
                                (Pat::Ident(binding), Expr::Arrow(arrow)) => {
                                    let name = Some(binding.id.sym.to_string());
                                    match arrow.body.as_ref() {
                                        BlockStmtOrExpr::BlockStmt(body) => self
                                            .lower_nested_function(
                                                arrow.params.clone(),
                                                &body.stmts,
                                                None,
                                                name,
                                                arrow.is_async,
                                                false,
                                                false,
                                                None,
                                            )?,
                                        BlockStmtOrExpr::Expr(expression) => self
                                            .lower_nested_function(
                                                arrow.params.clone(),
                                                &[],
                                                Some(expression.as_ref()),
                                                name,
                                                arrow.is_async,
                                                false,
                                                false,
                                                None,
                                            )?,
                                    }
                                }
                                _ => self.lower_expression(initializer)?,
                            };
                            self.initialize_binding_pattern(&declarator.name, value)?;
                        }
                        None if declaration.kind == VarDeclKind::Var => {
                            // Function-entry instantiation already initialized
                            // `var` to undefined. A declaration without an
                            // initializer must not overwrite an earlier write.
                        }
                        None => {
                            if !matches!(declarator.name, Pat::Ident(_)) {
                                return Err(anyhow!(
                                    "runtime W3IR destructuring declarations require an initializer"
                                ));
                            }
                            let value = self.constant(Constant::Undefined);
                            self.initialize_binding_pattern(&declarator.name, value)?;
                        }
                    }
                }
                Ok(())
            }
            Stmt::Decl(Decl::Fn(_)) => Ok(()),
            Stmt::Decl(Decl::Class(declaration)) => {
                let name = declaration.ident.sym.to_string();
                let binding = self
                    .find_binding(&name)
                    .ok_or_else(|| anyhow!("missing predeclared class binding {name:?}"))?;
                let value = self.lower_class_value(&declaration.class, Some(name))?;
                self.emit(Instruction::InitializeBinding { binding, value });
                Ok(())
            }
            Stmt::Return(statement) => {
                let value = if let Some(argument) = &statement.arg {
                    self.lower_expression(argument)?
                } else {
                    self.constant(Constant::Undefined)
                };
                self.lower_return_value(value)
            }
            Stmt::Throw(statement) => {
                let value = self.lower_expression(&statement.arg)?;
                self.terminate(Instruction::Throw { value });
                Ok(())
            }
            Stmt::If(statement) => self.lower_if_statement(statement),
            Stmt::While(statement) => self.lower_while_statement(statement),
            Stmt::DoWhile(statement) => self.lower_do_while_statement(statement),
            Stmt::For(statement) => self.lower_for_statement(statement),
            Stmt::ForIn(statement) => self.lower_for_in_statement(statement),
            Stmt::ForOf(statement) => self.lower_for_of_statement(statement),
            Stmt::Switch(statement) => self.lower_switch_statement(statement),
            Stmt::Try(statement) => self.lower_try_statement(statement),
            Stmt::Labeled(statement) => self.lower_labeled_statement(statement),
            Stmt::Break(statement) => {
                let target = if let Some(label) = &statement.label {
                    let label = label.sym.as_ref();
                    self.control_targets
                        .iter()
                        .rev()
                        .find(|target| target.labels.iter().any(|candidate| candidate == label))
                        .cloned()
                        .ok_or_else(|| anyhow!("unresolved runtime W3IR break label {label:?}"))?
                } else {
                    self.control_targets.last().cloned().ok_or_else(|| {
                        anyhow!("break used outside a runtime W3IR loop or switch")
                    })?
                };
                self.emit_control_transfer_cleanup(target.iterator_depth)?;
                self.lower_finalizers_for_transfer(target.finally_depth)?;
                if !self.terminated {
                    self.terminate(Instruction::Jump {
                        target: target.break_block,
                    });
                }
                Ok(())
            }
            Stmt::Continue(statement) => {
                let target = if let Some(label) = &statement.label {
                    let label = label.sym.as_ref();
                    let target = self
                        .control_targets
                        .iter()
                        .rev()
                        .find(|target| target.labels.iter().any(|candidate| candidate == label))
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!("unresolved runtime W3IR continue label {label:?}")
                        })?;
                    if target.continue_block.is_none() {
                        return Err(anyhow!(
                            "runtime W3IR continue label {label:?} does not target a loop"
                        ));
                    }
                    target
                } else {
                    self.control_targets
                        .iter()
                        .rev()
                        .find(|target| target.continue_block.is_some())
                        .cloned()
                        .ok_or_else(|| anyhow!("continue used outside a runtime W3IR loop"))?
                };
                self.emit_control_transfer_cleanup(target.iterator_depth)?;
                self.lower_finalizers_for_transfer(target.finally_depth)?;
                if !self.terminated {
                    self.terminate(Instruction::Jump {
                        target: target.continue_block.expect("validated loop target"),
                    });
                }
                Ok(())
            }
            _ => Err(anyhow!(
                "runtime W3IR lowering does not yet support this statement"
            )),
        }
    }

    fn lower_labeled_statement(&mut self, statement: &swc_ecma_ast::LabeledStmt) -> Result<()> {
        let label = statement.label.sym.to_string();
        if labeled_statement_targets_breakable(&statement.body) {
            let pending_len = self.pending_control_labels.len();
            self.pending_control_labels.push(label);
            let result = self.lower_statement(&statement.body);
            self.pending_control_labels.truncate(pending_len);
            return result;
        }

        let exit_block = self.allocate_block();
        self.control_targets.push(ControlTarget {
            labels: vec![label],
            break_block: exit_block,
            continue_block: None,
            iterator_depth: self.active_iterators.len(),
            finally_depth: self.active_finalizers.len(),
        });
        let result = self.lower_statement(&statement.body);
        self.control_targets.pop();
        result?;
        if !self.terminated {
            self.terminate(Instruction::Jump { target: exit_block });
        }
        self.seal_current_block();
        self.start_block(exit_block);
        Ok(())
    }

    fn emit_control_transfer_cleanup(&mut self, iterator_depth: usize) -> Result<()> {
        if self.active_iterators.len() <= iterator_depth {
            return Ok(());
        }
        let crossed = self.active_iterators[iterator_depth..]
            .iter()
            .rev()
            .map(|context| context.iterator)
            .collect::<Vec<_>>();
        let has_async = self.active_iterators[iterator_depth..]
            .iter()
            .any(|context| context.is_async);
        let mut removed = self.active_iterators.split_off(iterator_depth);
        let result = self.emit_iterator_close_chain(&crossed, None, has_async);
        self.active_iterators.append(&mut removed);
        result.map(|_| ())
    }

    fn lower_return_value(&mut self, value: Register) -> Result<()> {
        let iterators = self
            .active_iterators
            .iter()
            .rev()
            .map(|context| context.iterator)
            .collect::<Vec<_>>();
        let has_async_iterator = self.active_iterators.iter().any(|context| context.is_async);
        if !iterators.is_empty() {
            let cleanup_block = self.allocate_block();
            self.terminate(Instruction::Jump {
                target: cleanup_block,
            });
            self.seal_current_block();
            self.start_block(cleanup_block);
            let active_iterators = std::mem::take(&mut self.active_iterators);
            let _ = self.emit_iterator_close_chain(&iterators, None, has_async_iterator)?;
            self.active_iterators = active_iterators;
        }
        self.lower_finalizers_for_transfer(0)?;
        if !self.terminated {
            self.terminate(Instruction::Return { value });
        }
        Ok(())
    }

    /// Materialize the finally bodies crossed by an abrupt completion in
    /// innermost-to-outermost order. Each body starts in a fresh block outside
    /// the exception region it is completing, so a throw/return from finally
    /// correctly replaces the pending completion.
    fn lower_finalizers_for_transfer(&mut self, depth: usize) -> Result<()> {
        if self.active_finalizers.len() <= depth {
            return Ok(());
        }

        let cleanup_block = self.allocate_block();
        self.terminate(Instruction::Jump {
            target: cleanup_block,
        });
        self.seal_current_block();
        self.start_block(cleanup_block);

        let mut completed = Vec::new();
        while self.active_finalizers.len() > depth && !self.terminated {
            let finalizer = self
                .active_finalizers
                .pop()
                .expect("checked active finalizer");
            let suspended_protections = self
                .active_protections
                .split_off(finalizer.protection_depth);
            let result = self.lower_statement(&Stmt::Block(finalizer.body.clone()));
            self.active_protections.extend(suspended_protections);
            completed.push(finalizer);
            result?;
        }
        for finalizer in completed.into_iter().rev() {
            self.active_finalizers.push(finalizer);
        }
        Ok(())
    }

    fn lower_try_statement(&mut self, statement: &swc_ecma_ast::TryStmt) -> Result<()> {
        let try_block = self.allocate_block();
        let catch_block = statement.handler.as_ref().map(|_| self.allocate_block());
        let normal_finally_block = statement.finalizer.as_ref().map(|_| self.allocate_block());
        let exceptional_finally_block = statement.finalizer.as_ref().map(|_| self.allocate_block());
        let exit_block = self.allocate_block();
        let exception = self.register();

        self.terminate(Instruction::Jump { target: try_block });
        self.seal_current_block();

        let protection_depth = self.active_protections.len();
        if let Some(finalizer) = &statement.finalizer {
            self.active_finalizers.push(ActiveFinally {
                body: finalizer.clone(),
                protection_depth,
            });
        }

        self.start_block(try_block);
        self.active_protections.push(ActiveProtection::default());
        let try_result = self.lower_statement(&Stmt::Block(statement.block.clone()));
        let try_protection = self
            .active_protections
            .pop()
            .expect("try protection was pushed");
        try_result?;
        if !self.terminated {
            self.terminate(Instruction::Jump {
                target: normal_finally_block.unwrap_or(exit_block),
            });
        }
        self.seal_current_block();

        let mut catch_protection = None;
        if let (Some(handler), Some(handler_block)) = (&statement.handler, catch_block) {
            self.start_block(handler_block);
            if statement.finalizer.is_some() {
                self.active_protections.push(ActiveProtection::default());
            }
            self.scopes.push(HashMap::new());
            let catch_result = (|| {
                if let Some(parameter) = &handler.param {
                    self.declare_catch_pattern_bindings(parameter)?;
                    self.initialize_binding_pattern(parameter, exception)?;
                }
                self.lower_statement(&Stmt::Block(handler.body.clone()))
            })();
            self.scopes.pop();
            if statement.finalizer.is_some() {
                catch_protection = self.active_protections.pop();
            }
            catch_result?;
            if !self.terminated {
                self.terminate(Instruction::Jump {
                    target: normal_finally_block.unwrap_or(exit_block),
                });
            }
            self.seal_current_block();
        }

        if !try_protection.blocks.is_empty() {
            self.exception_regions.push(ExceptionRegion {
                protected_blocks: try_protection.blocks,
                catch_block,
                finally_block: if catch_block.is_none() {
                    Some(exceptional_finally_block.ok_or_else(|| {
                        anyhow!("runtime W3IR try statement requires catch or finally")
                    })?)
                } else {
                    None
                },
                exception,
            });
        }

        if let Some(finalizer) = &statement.finalizer {
            let active = self
                .active_finalizers
                .pop()
                .expect("try finalizer was pushed");
            debug_assert_eq!(active.body, *finalizer);

            if let Some(protection) = catch_protection
                && !protection.blocks.is_empty()
            {
                self.exception_regions.push(ExceptionRegion {
                    protected_blocks: protection.blocks,
                    catch_block: None,
                    finally_block: exceptional_finally_block,
                    exception,
                });
            }

            self.start_block(normal_finally_block.expect("finalizer block"));
            self.lower_statement(&Stmt::Block(finalizer.clone()))?;
            if !self.terminated {
                self.terminate(Instruction::Jump { target: exit_block });
            }
            self.seal_current_block();

            self.start_block(exceptional_finally_block.expect("exception finalizer block"));
            self.lower_statement(&Stmt::Block(finalizer.clone()))?;
            if !self.terminated {
                self.terminate(Instruction::Throw { value: exception });
            }
            self.seal_current_block();
        }

        self.start_block(exit_block);
        Ok(())
    }

    fn lower_if_statement(&mut self, statement: &swc_ecma_ast::IfStmt) -> Result<()> {
        let condition = self.lower_expression(&statement.test)?;
        let then_block = self.allocate_block();
        let else_block = self.allocate_block();
        let merge_block = self.allocate_block();
        self.terminate(Instruction::Branch {
            condition,
            then_block,
            else_block,
        });
        self.seal_current_block();

        self.start_block(then_block);
        self.lower_if_branch(&statement.cons)?;
        if !self.terminated {
            self.terminate(Instruction::Jump {
                target: merge_block,
            });
        }
        self.seal_current_block();

        self.start_block(else_block);
        if let Some(alternate) = &statement.alt {
            self.lower_if_branch(alternate)?;
        }
        if !self.terminated {
            self.terminate(Instruction::Jump {
                target: merge_block,
            });
        }
        self.seal_current_block();
        self.start_block(merge_block);
        Ok(())
    }

    fn lower_if_branch(&mut self, statement: &Stmt) -> Result<()> {
        if matches!(statement, Stmt::Decl(Decl::Fn(_))) {
            if !self.annex_b_function_declarations {
                return Err(anyhow!(
                    "Annex B branch-level function declarations require a non-strict classic script"
                ));
            }
            return self.initialize_function_declaration(statement);
        }
        self.lower_statement(statement)
    }

    fn lower_while_statement(&mut self, statement: &swc_ecma_ast::WhileStmt) -> Result<()> {
        let labels = std::mem::take(&mut self.pending_control_labels);
        let condition_block = self.allocate_block();
        let body_block = self.allocate_block();
        let exit_block = self.allocate_block();
        self.terminate(Instruction::Jump {
            target: condition_block,
        });
        self.seal_current_block();

        self.start_block(condition_block);
        let condition = self.lower_expression(&statement.test)?;
        self.terminate(Instruction::Branch {
            condition,
            then_block: body_block,
            else_block: exit_block,
        });
        self.seal_current_block();

        self.start_block(body_block);
        self.control_targets.push(ControlTarget {
            labels,
            break_block: exit_block,
            continue_block: Some(condition_block),
            iterator_depth: self.active_iterators.len(),
            finally_depth: self.active_finalizers.len(),
        });
        let body_result = self.lower_statement(&statement.body);
        self.control_targets.pop();
        body_result?;
        if !self.terminated {
            self.terminate(Instruction::Jump {
                target: condition_block,
            });
        }
        self.seal_current_block();
        self.start_block(exit_block);
        Ok(())
    }

    fn lower_do_while_statement(&mut self, statement: &swc_ecma_ast::DoWhileStmt) -> Result<()> {
        let labels = std::mem::take(&mut self.pending_control_labels);
        let body_block = self.allocate_block();
        let condition_block = self.allocate_block();
        let exit_block = self.allocate_block();
        self.terminate(Instruction::Jump { target: body_block });
        self.seal_current_block();

        self.start_block(body_block);
        self.control_targets.push(ControlTarget {
            labels,
            break_block: exit_block,
            continue_block: Some(condition_block),
            iterator_depth: self.active_iterators.len(),
            finally_depth: self.active_finalizers.len(),
        });
        let body_result = self.lower_statement(&statement.body);
        self.control_targets.pop();
        body_result?;
        if !self.terminated {
            self.terminate(Instruction::Jump {
                target: condition_block,
            });
        }
        self.seal_current_block();

        self.start_block(condition_block);
        let condition = self.lower_expression(&statement.test)?;
        self.terminate(Instruction::Branch {
            condition,
            then_block: body_block,
            else_block: exit_block,
        });
        self.seal_current_block();
        self.start_block(exit_block);
        Ok(())
    }

    fn lower_for_statement(&mut self, statement: &swc_ecma_ast::ForStmt) -> Result<()> {
        let labels = std::mem::take(&mut self.pending_control_labels);
        self.scopes.push(HashMap::new());
        let result = (|| {
            let mut per_iteration_bindings = Vec::new();
            if let Some(initializer) = &statement.init {
                match initializer {
                    swc_ecma_ast::VarDeclOrExpr::VarDecl(declaration) => {
                        if declaration.kind != VarDeclKind::Var {
                            for declarator in &declaration.decls {
                                self.declare_variable_pattern_bindings(
                                    &declarator.name,
                                    declaration.kind,
                                    &mut per_iteration_bindings,
                                )?;
                            }
                        }
                        self.lower_statement(&Stmt::Decl(Decl::Var(declaration.clone())))?;
                    }
                    swc_ecma_ast::VarDeclOrExpr::Expr(expression) => {
                        self.lower_expression(expression)?;
                    }
                }
            }
            for binding in &per_iteration_bindings {
                self.emit(Instruction::RefreshBinding { binding: *binding });
            }

            let condition_block = self.allocate_block();
            let body_block = self.allocate_block();
            let update_block = self.allocate_block();
            let exit_block = self.allocate_block();
            self.terminate(Instruction::Jump {
                target: condition_block,
            });
            self.seal_current_block();

            self.start_block(condition_block);
            let condition = if let Some(test) = &statement.test {
                self.lower_expression(test)?
            } else {
                self.constant(Constant::Bool(true))
            };
            self.terminate(Instruction::Branch {
                condition,
                then_block: body_block,
                else_block: exit_block,
            });
            self.seal_current_block();

            self.start_block(body_block);
            self.control_targets.push(ControlTarget {
                labels,
                break_block: exit_block,
                continue_block: Some(update_block),
                iterator_depth: self.active_iterators.len(),
                finally_depth: self.active_finalizers.len(),
            });
            let body_result = self.lower_statement(&statement.body);
            self.control_targets.pop();
            body_result?;
            if !self.terminated {
                self.terminate(Instruction::Jump {
                    target: update_block,
                });
            }
            self.seal_current_block();

            self.start_block(update_block);
            for binding in &per_iteration_bindings {
                self.emit(Instruction::RefreshBinding { binding: *binding });
            }
            if let Some(update) = &statement.update {
                self.lower_expression(update)?;
            }
            self.terminate(Instruction::Jump {
                target: condition_block,
            });
            self.seal_current_block();
            self.start_block(exit_block);
            Ok(())
        })();
        self.scopes.pop();
        result
    }

    fn lower_for_of_statement(&mut self, statement: &swc_ecma_ast::ForOfStmt) -> Result<()> {
        let labels = std::mem::take(&mut self.pending_control_labels);
        if statement.is_await && !self.allows_await {
            return Err(anyhow!(
                "for-await-of is only supported inside async functions or modules"
            ));
        }
        self.lower_iterator_statement(
            &statement.left,
            &statement.body,
            statement.is_await,
            labels,
            "for-of",
            |builder| builder.lower_expression(&statement.right),
        )
    }

    fn lower_for_in_statement(&mut self, statement: &swc_ecma_ast::ForInStmt) -> Result<()> {
        let labels = std::mem::take(&mut self.pending_control_labels);
        self.lower_iterator_statement(
            &statement.left,
            &statement.body,
            false,
            labels,
            "for-in",
            |builder| {
                let value = builder.lower_expression(&statement.right)?;
                let key = builder.constant(Constant::String("__w3cos_for_in_keys".to_string()));
                let keys = builder.register();
                builder.emit(Instruction::CallMethod {
                    dst: keys,
                    object: value,
                    key,
                    arguments: Vec::new(),
                });
                Ok(keys)
            },
        )
    }

    fn lower_iterator_statement(
        &mut self,
        left: &ForHead,
        body: &Stmt,
        is_async: bool,
        labels: Vec<String>,
        loop_name: &str,
        lower_iterable: impl FnOnce(&mut Self) -> Result<Register>,
    ) -> Result<()> {
        self.scopes.push(HashMap::new());
        let result = (|| {
            let mut per_iteration_bindings = Vec::new();
            let declaration = match left {
                ForHead::VarDecl(declaration) => {
                    if declaration.decls.len() != 1 || declaration.decls[0].init.is_some() {
                        return Err(anyhow!(
                            "runtime W3IR {loop_name} requires one declaration without an initializer"
                        ));
                    }
                    if declaration.kind != VarDeclKind::Var {
                        self.declare_variable_pattern_bindings(
                            &declaration.decls[0].name,
                            declaration.kind,
                            &mut per_iteration_bindings,
                        )?;
                    }
                    Some(declaration.as_ref())
                }
                ForHead::Pat(_) => None,
                ForHead::UsingDecl(_) => {
                    return Err(anyhow!(
                        "using declarations are not yet supported in runtime W3IR {loop_name}"
                    ));
                }
            };

            let iterable = lower_iterable(self)?;
            let iterator_key = self.constant(Constant::String(
                if is_async {
                    "__w3cos_symbol_async_iterator"
                } else {
                    "__w3cos_symbol_iterator"
                }
                .to_string(),
            ));
            let iterator = self.register();
            self.emit(Instruction::CallMethod {
                dst: iterator,
                object: iterable,
                key: iterator_key,
                arguments: Vec::new(),
            });

            let condition_block = self.allocate_block();
            let body_block = self.allocate_block();
            let close_block = self.allocate_block();
            let exception_close_block = self.allocate_block();
            let exit_block = self.allocate_block();
            self.terminate(Instruction::Jump {
                target: condition_block,
            });
            self.seal_current_block();

            self.start_block(condition_block);
            let next_key = self.constant(Constant::String(
                if is_async {
                    "__w3cos_async_iterator_next"
                } else {
                    "__w3cos_iterator_next"
                }
                .to_string(),
            ));
            let next_call = self.register();
            self.emit(Instruction::CallMethod {
                dst: next_call,
                object: iterator,
                key: next_key,
                arguments: Vec::new(),
            });
            let next = if is_async {
                self.emit_await(next_call)?
            } else {
                next_call
            };
            let done_key = self.constant(Constant::String("done".to_string()));
            let done = self.register();
            self.emit(Instruction::GetProperty {
                dst: done,
                object: next,
                key: done_key,
            });
            self.terminate(Instruction::Branch {
                condition: done,
                then_block: exit_block,
                else_block: body_block,
            });
            self.seal_current_block();

            self.start_block(body_block);
            self.active_iterators.push(ActiveIterator {
                iterator,
                is_async,
                protected_blocks: Vec::new(),
            });
            let body_result = (|| {
                let value_key = self.constant(Constant::String("value".to_string()));
                let raw_value = self.register();
                self.emit(Instruction::GetProperty {
                    dst: raw_value,
                    object: next,
                    key: value_key,
                });
                let value = if is_async {
                    self.emit_await(raw_value)?
                } else {
                    raw_value
                };
                for binding in &per_iteration_bindings {
                    self.emit(Instruction::RefreshBinding { binding: *binding });
                }
                match (left, declaration) {
                    (ForHead::VarDecl(_), Some(declaration)) => {
                        self.initialize_binding_pattern(&declaration.decls[0].name, value)?;
                    }
                    (ForHead::Pat(pattern), None) => {
                        self.assign_binding_pattern(pattern, value)?;
                    }
                    _ => unreachable!("for-of head classified above"),
                }

                self.control_targets.push(ControlTarget {
                    labels,
                    break_block: close_block,
                    continue_block: Some(condition_block),
                    iterator_depth: self.active_iterators.len(),
                    finally_depth: self.active_finalizers.len(),
                });
                let result = self.lower_statement(body);
                self.control_targets.pop();
                result
            })();
            let iterator_context = self
                .active_iterators
                .pop()
                .expect("for-of iterator context");
            body_result?;
            if !self.terminated {
                self.terminate(Instruction::Jump {
                    target: condition_block,
                });
            }
            self.seal_current_block();
            let exception = self.register();
            if !iterator_context.protected_blocks.is_empty() {
                self.exception_regions.push(ExceptionRegion {
                    protected_blocks: iterator_context.protected_blocks,
                    catch_block: Some(exception_close_block),
                    finally_block: None,
                    exception,
                });
            }

            self.start_block(close_block);
            let _ = self.emit_iterator_close_chain(&[iterator], None, is_async)?;
            self.terminate(Instruction::Jump { target: exit_block });
            self.seal_current_block();

            self.start_block(exception_close_block);
            let exception = self
                .emit_iterator_close_chain(&[iterator], Some(exception), is_async)?
                .unwrap_or(exception);
            self.terminate(Instruction::Throw { value: exception });
            self.seal_current_block();

            self.start_block(exit_block);
            Ok(())
        })();
        self.scopes.pop();
        result
    }

    fn emit_iterator_close_chain(
        &mut self,
        iterators: &[Register],
        pending_throw: Option<Register>,
        is_async: bool,
    ) -> Result<Option<Register>> {
        if iterators.is_empty() {
            return Ok(pending_throw);
        }
        let chain = self.register();
        self.emit(Instruction::CreateArray {
            dst: chain,
            elements: iterators.to_vec(),
        });
        let operation = if is_async && pending_throw.is_some() {
            "__w3cos_async_iterator_close_throw"
        } else if is_async {
            "__w3cos_async_iterator_close_return"
        } else if pending_throw.is_some() {
            "__w3cos_iterator_close_throw"
        } else {
            "__w3cos_iterator_close_return"
        };
        let key = self.constant(Constant::String(operation.to_string()));
        let ignored = self.register();
        self.emit(Instruction::CallMethod {
            dst: ignored,
            object: chain,
            key,
            arguments: pending_throw.into_iter().collect(),
        });
        if is_async {
            return self.emit_await(ignored).map(Some);
        }
        Ok(Some(ignored))
    }

    fn lower_switch_statement(&mut self, statement: &swc_ecma_ast::SwitchStmt) -> Result<()> {
        let labels = std::mem::take(&mut self.pending_control_labels);
        let discriminant = self.lower_expression(&statement.discriminant)?;
        self.scopes.push(HashMap::new());
        let result = (|| {
            for case in &statement.cases {
                for statement in &case.cons {
                    self.predeclare_lexical_binding(statement)?;
                    self.predeclare_block_function_binding(statement)?;
                }
            }
            let mut switch_bindings = self
                .scopes
                .last()
                .unwrap()
                .values()
                .copied()
                .collect::<Vec<_>>();
            switch_bindings.sort_by_key(|binding| binding.0);
            for binding in switch_bindings {
                self.emit(Instruction::RefreshBinding { binding });
            }
            for case in &statement.cases {
                for statement in &case.cons {
                    self.initialize_function_declaration(statement)?;
                }
            }

            let exit_block = self.allocate_block();
            let case_blocks = statement
                .cases
                .iter()
                .map(|_| self.allocate_block())
                .collect::<Vec<_>>();
            let default_block = statement
                .cases
                .iter()
                .position(|case| case.test.is_none())
                .map(|index| case_blocks[index])
                .unwrap_or(exit_block);

            for (index, case) in statement.cases.iter().enumerate() {
                let Some(test) = &case.test else {
                    continue;
                };
                let test = self.lower_expression(test)?;
                let matches = self.register();
                self.emit(Instruction::Binary {
                    dst: matches,
                    operator: BinaryOperator::StrictEqual,
                    lhs: discriminant,
                    rhs: test,
                });
                let next_dispatch = self.allocate_block();
                self.terminate(Instruction::Branch {
                    condition: matches,
                    then_block: case_blocks[index],
                    else_block: next_dispatch,
                });
                self.seal_current_block();
                self.start_block(next_dispatch);
            }
            self.terminate(Instruction::Jump {
                target: default_block,
            });
            self.seal_current_block();

            self.control_targets.push(ControlTarget {
                labels,
                break_block: exit_block,
                continue_block: None,
                iterator_depth: self.active_iterators.len(),
                finally_depth: self.active_finalizers.len(),
            });
            for (index, case) in statement.cases.iter().enumerate() {
                self.start_block(case_blocks[index]);
                for statement in &case.cons {
                    self.lower_statement(statement)?;
                }
                if !self.terminated {
                    self.terminate(Instruction::Jump {
                        target: case_blocks.get(index + 1).copied().unwrap_or(exit_block),
                    });
                }
                self.seal_current_block();
            }
            self.control_targets.pop();
            self.start_block(exit_block);
            Ok(())
        })();
        self.scopes.pop();
        result
    }

    fn lower_branch_value(
        &mut self,
        condition: Register,
        consequent: impl FnOnce(&mut Self) -> Result<Register>,
        alternate: impl FnOnce(&mut Self) -> Result<Register>,
    ) -> Result<Register> {
        let consequent_block = self.allocate_block();
        let alternate_block = self.allocate_block();
        let merge_block = self.allocate_block();
        let result = self.register();
        self.terminate(Instruction::Branch {
            condition,
            then_block: consequent_block,
            else_block: alternate_block,
        });
        self.seal_current_block();

        self.start_block(consequent_block);
        let consequent = consequent(self)?;
        if !self.terminated {
            self.emit(Instruction::Move {
                dst: result,
                src: consequent,
            });
            self.terminate(Instruction::Jump {
                target: merge_block,
            });
        }
        self.seal_current_block();

        self.start_block(alternate_block);
        let alternate = alternate(self)?;
        if !self.terminated {
            self.emit(Instruction::Move {
                dst: result,
                src: alternate,
            });
            self.terminate(Instruction::Jump {
                target: merge_block,
            });
        }
        self.seal_current_block();

        self.start_block(merge_block);
        Ok(result)
    }

    fn lower_logical_expression(&mut self, expression: &swc_ecma_ast::BinExpr) -> Result<Register> {
        let lhs = self.lower_expression(&expression.left)?;
        match expression.op {
            BinaryOp::LogicalAnd => self.lower_branch_value(
                lhs,
                |builder| builder.lower_expression(&expression.right),
                |_| Ok(lhs),
            ),
            BinaryOp::LogicalOr => self.lower_branch_value(
                lhs,
                |_| Ok(lhs),
                |builder| builder.lower_expression(&expression.right),
            ),
            BinaryOp::NullishCoalescing => {
                let null = self.constant(Constant::Null);
                let is_nullish = self.register();
                self.emit(Instruction::Binary {
                    dst: is_nullish,
                    operator: BinaryOperator::AbstractEqual,
                    lhs,
                    rhs: null,
                });
                self.lower_branch_value(
                    is_nullish,
                    |builder| builder.lower_expression(&expression.right),
                    |_| Ok(lhs),
                )
            }
            _ => unreachable!("logical lowering called for a non-logical operator"),
        }
    }

    fn lower_conditional_expression(
        &mut self,
        expression: &swc_ecma_ast::CondExpr,
    ) -> Result<Register> {
        let condition = self.lower_expression(&expression.test)?;
        self.lower_branch_value(
            condition,
            |builder| builder.lower_expression(&expression.cons),
            |builder| builder.lower_expression(&expression.alt),
        )
    }

    fn lower_unary_expression(&mut self, expression: &swc_ecma_ast::UnaryExpr) -> Result<Register> {
        if expression.op == UnaryOp::Delete {
            if let Expr::Member(member) = expression.arg.as_ref() {
                if matches!(member.prop, MemberProp::PrivateName(_)) {
                    return Err(anyhow!("private fields cannot be deleted"));
                }
                let (object, key) = self.lower_member_parts(member)?;
                let dst = self.register();
                self.emit(Instruction::DeleteProperty { dst, object, key });
                return Ok(dst);
            }

            // Non-reference delete operands are still evaluated for side
            // effects and then produce true.
            self.lower_expression(&expression.arg)?;
            return Ok(self.constant(Constant::Bool(true)));
        }

        let argument = self.lower_expression(&expression.arg)?;
        match expression.op {
            UnaryOp::Bang => self.lower_branch_value(
                argument,
                |builder| Ok(builder.constant(Constant::Bool(false))),
                |builder| Ok(builder.constant(Constant::Bool(true))),
            ),
            UnaryOp::Plus => {
                let one = self.constant(Constant::Number(1.0));
                let dst = self.register();
                self.emit(Instruction::Binary {
                    dst,
                    operator: BinaryOperator::Multiply,
                    lhs: argument,
                    rhs: one,
                });
                Ok(dst)
            }
            UnaryOp::Minus => {
                let dst = self.register();
                self.emit(Instruction::Unary {
                    dst,
                    operator: UnaryOperator::Negate,
                    value: argument,
                });
                Ok(dst)
            }
            UnaryOp::TypeOf => {
                let dst = self.register();
                self.emit(Instruction::Unary {
                    dst,
                    operator: UnaryOperator::TypeOf,
                    value: argument,
                });
                Ok(dst)
            }
            UnaryOp::Tilde => {
                let dst = self.register();
                self.emit(Instruction::Unary {
                    dst,
                    operator: UnaryOperator::BitwiseNot,
                    value: argument,
                });
                Ok(dst)
            }
            UnaryOp::Void => Ok(self.constant(Constant::Undefined)),
            _ => Err(anyhow!(
                "runtime W3IR does not yet support unary operator {:?}",
                expression.op
            )),
        }
    }

    fn ensure_mutable_binding(&self, name: &str, binding: BindingId) -> Result<()> {
        if self
            .bindings
            .iter()
            .find(|candidate| candidate.id == binding)
            .is_some_and(|binding| !binding.mutable)
        {
            Err(anyhow!(
                "assignment to immutable runtime binding {name:?} is not allowed"
            ))
        } else {
            Ok(())
        }
    }

    fn emit_arithmetic(
        &mut self,
        operator: BinaryOp,
        lhs: Register,
        rhs: Register,
    ) -> Result<Register> {
        let dst = self.register();
        if operator == BinaryOp::Add {
            self.emit(Instruction::Add { dst, lhs, rhs });
            return Ok(dst);
        }
        let operator = match operator {
            BinaryOp::Sub => BinaryOperator::Subtract,
            BinaryOp::Mul => BinaryOperator::Multiply,
            BinaryOp::Div => BinaryOperator::Divide,
            BinaryOp::Mod => BinaryOperator::Remainder,
            BinaryOp::Exp => BinaryOperator::Exponentiate,
            BinaryOp::LShift => BinaryOperator::LeftShift,
            BinaryOp::RShift => BinaryOperator::SignedRightShift,
            BinaryOp::ZeroFillRShift => BinaryOperator::UnsignedRightShift,
            BinaryOp::BitOr => BinaryOperator::BitwiseOr,
            BinaryOp::BitXor => BinaryOperator::BitwiseXor,
            BinaryOp::BitAnd => BinaryOperator::BitwiseAnd,
            _ => {
                return Err(anyhow!(
                    "runtime W3IR update operation does not yet support this operator"
                ));
            }
        };
        self.emit(Instruction::Binary {
            dst,
            operator,
            lhs,
            rhs,
        });
        Ok(dst)
    }

    fn lower_update_expression(
        &mut self,
        expression: &swc_ecma_ast::UpdateExpr,
    ) -> Result<Register> {
        let one = self.constant(Constant::Number(1.0));
        let target = match expression.arg.as_ref() {
            Expr::Ident(identifier) => {
                let name = identifier.sym.to_string();
                self.find_binding(&name).ok_or_else(|| {
                    anyhow!("update of undeclared runtime binding {name:?} is not supported")
                })?;
                let binding = self.resolve_binding(&name);
                let mutable = self
                    .bindings
                    .iter()
                    .find(|candidate| candidate.id == binding)
                    .is_none_or(|binding| binding.mutable);
                LoweredAssignmentTarget::Binding { binding, mutable }
            }
            Expr::Member(member) => {
                if matches!(member.prop, MemberProp::PrivateName(_)) {
                    let (object, brand, name) = self.lower_private_member_parts(member)?;
                    LoweredAssignmentTarget::Private {
                        object,
                        brand,
                        name,
                    }
                } else {
                    let (object, key) = self.lower_member_parts(member)?;
                    LoweredAssignmentTarget::Property { object, key }
                }
            }
            Expr::SuperProp(property) => self.lower_super_assignment_target(property)?,
            _ => {
                return Err(anyhow!(
                    "runtime W3IR update requires an identifier, member, or super target"
                ));
            }
        };
        self.ensure_assignment_target_mutable(target)?;
        let previous = self.load_assignment_target(target);
        let operator = match expression.op {
            UpdateOp::PlusPlus => BinaryOp::Add,
            UpdateOp::MinusMinus => BinaryOp::Sub,
        };
        let numeric_previous = self.emit_arithmetic(BinaryOp::Mul, previous, one)?;
        let updated = self.emit_arithmetic(operator, numeric_previous, one)?;
        self.store_assignment_target(target, updated);
        Ok(if expression.prefix {
            updated
        } else {
            numeric_previous
        })
    }

    fn lower_compound_assignment(
        &mut self,
        assignment: &swc_ecma_ast::AssignExpr,
    ) -> Result<Register> {
        let operator = assignment.op.to_update().ok_or_else(|| {
            anyhow!("runtime W3IR compound assignment requires an update operator")
        })?;
        let target = self.lower_assignment_target(&assignment.left)?;
        self.ensure_assignment_target_mutable(target)?;
        let previous = self.load_assignment_target(target);
        let rhs = self.lower_expression(&assignment.right)?;
        let updated = self.emit_arithmetic(operator, previous, rhs)?;
        self.store_assignment_target(target, updated);
        Ok(updated)
    }

    fn lower_assignment_target(
        &mut self,
        target: &AssignTarget,
    ) -> Result<LoweredAssignmentTarget> {
        match target {
            AssignTarget::Simple(SimpleAssignTarget::Ident(identifier)) => {
                let name = identifier.id.sym.to_string();
                self.find_binding(&name).ok_or_else(|| {
                    anyhow!("assignment to undeclared runtime binding {name:?} is not supported")
                })?;
                let binding = self.resolve_binding(&name);
                let mutable = self
                    .bindings
                    .iter()
                    .find(|candidate| candidate.id == binding)
                    .is_none_or(|binding| binding.mutable);
                Ok(LoweredAssignmentTarget::Binding { binding, mutable })
            }
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                if matches!(member.prop, MemberProp::PrivateName(_)) {
                    let (object, brand, name) = self.lower_private_member_parts(member)?;
                    Ok(LoweredAssignmentTarget::Private {
                        object,
                        brand,
                        name,
                    })
                } else {
                    let (object, key) = self.lower_member_parts(member)?;
                    Ok(LoweredAssignmentTarget::Property { object, key })
                }
            }
            AssignTarget::Simple(SimpleAssignTarget::SuperProp(property)) => {
                self.lower_super_assignment_target(property)
            }
            _ => Err(anyhow!(
                "runtime W3IR assignment requires an identifier, member, or super target"
            )),
        }
    }

    fn ensure_assignment_target_mutable(&self, target: LoweredAssignmentTarget) -> Result<()> {
        if matches!(
            target,
            LoweredAssignmentTarget::Binding { mutable: false, .. }
        ) {
            Err(anyhow!(
                "assignment to immutable runtime binding is not allowed"
            ))
        } else {
            Ok(())
        }
    }

    fn load_assignment_target(&mut self, target: LoweredAssignmentTarget) -> Register {
        match target {
            LoweredAssignmentTarget::Binding { binding, .. } => self.load_binding(binding),
            LoweredAssignmentTarget::Property { object, key } => {
                let dst = self.register();
                self.emit(Instruction::GetProperty { dst, object, key });
                dst
            }
            LoweredAssignmentTarget::Private {
                object,
                brand,
                name,
            } => {
                let dst = self.register();
                self.emit(Instruction::GetPrivate {
                    dst,
                    object,
                    brand,
                    name,
                });
                dst
            }
            LoweredAssignmentTarget::Super {
                parent,
                receiver,
                key: property,
                is_static,
            } => {
                let bridge = if is_static {
                    "__w3cos_static_super_get"
                } else {
                    "__w3cos_super_get"
                };
                let key = self.constant(Constant::String(bridge.into()));
                let dst = self.register();
                self.emit(Instruction::CallMethod {
                    dst,
                    object: parent,
                    key,
                    arguments: vec![receiver, property],
                });
                dst
            }
        }
    }

    fn store_assignment_target(&mut self, target: LoweredAssignmentTarget, value: Register) {
        match target {
            LoweredAssignmentTarget::Binding {
                binding,
                mutable: true,
            } => {
                self.emit(Instruction::StoreBinding { binding, value });
            }
            LoweredAssignmentTarget::Binding { mutable: false, .. } => {
                let error = self.constant(Constant::String(
                    "TypeError: Assignment to constant variable.".into(),
                ));
                self.terminate(Instruction::Throw { value: error });
            }
            LoweredAssignmentTarget::Property { object, key } => {
                self.emit(Instruction::SetProperty { object, key, value });
            }
            LoweredAssignmentTarget::Private {
                object,
                brand,
                name,
            } => {
                self.emit(Instruction::SetPrivate {
                    object,
                    brand,
                    name,
                    value,
                });
            }
            LoweredAssignmentTarget::Super {
                parent,
                receiver,
                key: property,
                is_static,
            } => {
                let bridge = if is_static {
                    "__w3cos_static_super_set"
                } else {
                    "__w3cos_super_set"
                };
                let key = self.constant(Constant::String(bridge.into()));
                let dst = self.register();
                self.emit(Instruction::CallMethod {
                    dst,
                    object: parent,
                    key,
                    arguments: vec![receiver, property, value],
                });
            }
        }
    }

    fn lower_simple_assignment(
        &mut self,
        assignment: &swc_ecma_ast::AssignExpr,
    ) -> Result<Register> {
        let target = self.lower_assignment_target(&assignment.left)?;
        self.ensure_assignment_target_mutable(target)?;
        let value = self.lower_expression(&assignment.right)?;
        self.store_assignment_target(target, value);
        Ok(value)
    }

    fn lower_destructuring_assignment(
        &mut self,
        assignment: &swc_ecma_ast::AssignExpr,
    ) -> Result<Register> {
        let AssignTarget::Pat(pattern) = &assignment.left else {
            return Err(anyhow!(
                "runtime W3IR destructuring assignment requires an array or object pattern"
            ));
        };
        let value = self.lower_expression(&assignment.right)?;
        self.assign_binding_pattern(&pattern.clone().into(), value)?;
        Ok(value)
    }

    fn lower_logical_assignment(
        &mut self,
        assignment: &swc_ecma_ast::AssignExpr,
    ) -> Result<Register> {
        let target = self.lower_assignment_target(&assignment.left)?;
        let previous = self.load_assignment_target(target);
        match assignment.op {
            AssignOp::AndAssign => self.lower_branch_value(
                previous,
                |builder| {
                    let value = builder.lower_expression(&assignment.right)?;
                    builder.store_assignment_target(target, value);
                    Ok(value)
                },
                |_| Ok(previous),
            ),
            AssignOp::OrAssign => self.lower_branch_value(
                previous,
                |_| Ok(previous),
                |builder| {
                    let value = builder.lower_expression(&assignment.right)?;
                    builder.store_assignment_target(target, value);
                    Ok(value)
                },
            ),
            AssignOp::NullishAssign => {
                let null = self.constant(Constant::Null);
                let is_nullish = self.register();
                self.emit(Instruction::Binary {
                    dst: is_nullish,
                    operator: BinaryOperator::AbstractEqual,
                    lhs: previous,
                    rhs: null,
                });
                self.lower_branch_value(
                    is_nullish,
                    |builder| {
                        let value = builder.lower_expression(&assignment.right)?;
                        builder.store_assignment_target(target, value);
                        Ok(value)
                    },
                    |_| Ok(previous),
                )
            }
            _ => Err(anyhow!(
                "runtime W3IR logical assignment requires &&=, ||= or ??="
            )),
        }
    }

    fn lower_expression(&mut self, expression: &Expr) -> Result<Register> {
        match expression {
            Expr::Ident(identifier) => {
                let name = identifier.sym.to_string();
                let binding = self.resolve_binding(&name);
                Ok(self.load_binding(binding))
            }
            Expr::Lit(literal) => self.lower_literal(literal),
            Expr::Member(member) => {
                let dst = self.register();
                if matches!(member.prop, MemberProp::PrivateName(_)) {
                    let (object, brand, name) = self.lower_private_member_parts(member)?;
                    self.emit(Instruction::GetPrivate {
                        dst,
                        object,
                        brand,
                        name,
                    });
                } else {
                    let (object, key) = self.lower_member_parts(member)?;
                    self.emit(Instruction::GetProperty { dst, object, key });
                }
                Ok(dst)
            }
            Expr::JSXElement(element) => self.lower_jsx_element(element),
            Expr::JSXFragment(fragment) => self.lower_jsx_children(&fragment.children),
            Expr::Object(object) => {
                let dst = self.register();
                self.emit(Instruction::CreateObject {
                    dst,
                    properties: Vec::new(),
                });
                for property in &object.props {
                    match property {
                        PropOrSpread::Spread(spread) => {
                            let source = self.lower_expression(&spread.expr)?;
                            self.emit(Instruction::CopyDataProperties {
                                target: dst,
                                source,
                            });
                        }
                        PropOrSpread::Prop(property) => {
                            let (key, value) = match property.as_ref() {
                                Prop::KeyValue(property) => (
                                    self.lower_property_name(&property.key)?,
                                    self.lower_expression(&property.value)?,
                                ),
                                Prop::Shorthand(identifier) => (
                                    self.constant(Constant::String(identifier.sym.to_string())),
                                    self.lower_expression(&Expr::Ident(identifier.clone()))?,
                                ),
                                Prop::Method(method) => {
                                    let key = self.lower_property_name(&method.key)?;
                                    let body = method.function.body.as_ref().ok_or_else(|| {
                                        anyhow!("runtime W3IR object method has no body")
                                    })?;
                                    let parameters = method
                                        .function
                                        .params
                                        .iter()
                                        .map(|parameter| parameter.pat.clone())
                                        .collect();
                                    let value = self.lower_nested_function(
                                        parameters,
                                        &body.stmts,
                                        None,
                                        object_property_name(&method.key),
                                        method.function.is_async,
                                        method.function.is_generator,
                                        true,
                                        None,
                                    )?;
                                    (key, value)
                                }
                                Prop::Getter(getter) => {
                                    let key = self.lower_prefixed_property_name(
                                        "__w3cos_getter_",
                                        &getter.key,
                                    )?;
                                    let statements = getter
                                        .body
                                        .as_ref()
                                        .map(|body| body.stmts.as_slice())
                                        .unwrap_or_default();
                                    let value = self.lower_nested_function(
                                        Vec::new(),
                                        statements,
                                        None,
                                        object_property_name(&getter.key)
                                            .map(|name| format!("get {name}")),
                                        false,
                                        false,
                                        true,
                                        None,
                                    )?;
                                    (key, value)
                                }
                                Prop::Setter(setter) => {
                                    let key = self.lower_prefixed_property_name(
                                        "__w3cos_setter_",
                                        &setter.key,
                                    )?;
                                    let statements = setter
                                        .body
                                        .as_ref()
                                        .map(|body| body.stmts.as_slice())
                                        .unwrap_or_default();
                                    let value = self.lower_nested_function(
                                        vec![setter.param.as_ref().clone()],
                                        statements,
                                        None,
                                        object_property_name(&setter.key)
                                            .map(|name| format!("set {name}")),
                                        false,
                                        false,
                                        true,
                                        None,
                                    )?;
                                    (key, value)
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "runtime W3IR does not support assignment properties in object literals"
                                    ));
                                }
                            };
                            self.emit(Instruction::DefineField {
                                object: dst,
                                key,
                                value,
                            });
                        }
                    }
                }
                Ok(dst)
            }
            Expr::Array(array) => {
                let has_spread = array
                    .elems
                    .iter()
                    .flatten()
                    .any(|element| element.spread.is_some());
                let has_holes = array.elems.iter().any(Option::is_none);
                if has_spread || has_holes {
                    let dst = self.register();
                    self.emit(Instruction::CreateArray {
                        dst,
                        elements: Vec::new(),
                    });
                    for element in &array.elems {
                        let Some(element) = element else {
                            self.append_array_hole(dst);
                            continue;
                        };
                        let value = self.lower_expression(&element.expr)?;
                        if element.spread.is_some() {
                            self.emit(Instruction::AppendIterable {
                                array: dst,
                                iterable: value,
                            });
                        } else {
                            self.emit(Instruction::AppendArrayElement { array: dst, value });
                        }
                    }
                    return Ok(dst);
                }
                let mut elements = Vec::with_capacity(array.elems.len());
                for element in &array.elems {
                    let element = element.as_ref().expect("holes use incremental lowering");
                    elements.push(self.lower_expression(&element.expr)?);
                }
                let dst = self.register();
                self.emit(Instruction::CreateArray { dst, elements });
                Ok(dst)
            }
            Expr::Tpl(template) => self.lower_template_literal(template),
            Expr::TsTypeAssertion(assertion) => self.lower_expression(&assertion.expr),
            Expr::TsNonNull(non_null) => self.lower_expression(&non_null.expr),
            Expr::TsAs(as_expression) => self.lower_expression(&as_expression.expr),
            Expr::TsConstAssertion(assertion) => self.lower_expression(&assertion.expr),
            Expr::TsInstantiation(instantiation) => self.lower_expression(&instantiation.expr),
            Expr::TsSatisfies(satisfies) => self.lower_expression(&satisfies.expr),
            Expr::Arrow(arrow) => {
                if arrow.is_generator {
                    return Err(anyhow!(
                        "generator callbacks are not yet supported in runtime W3IR"
                    ));
                }
                let parameters = arrow.params.clone();
                match arrow.body.as_ref() {
                    BlockStmtOrExpr::BlockStmt(body) => self.lower_nested_function(
                        parameters,
                        &body.stmts,
                        None,
                        None,
                        arrow.is_async,
                        false,
                        false,
                        None,
                    ),
                    BlockStmtOrExpr::Expr(expression) => self.lower_nested_function(
                        parameters,
                        &[],
                        Some(expression.as_ref()),
                        None,
                        arrow.is_async,
                        false,
                        false,
                        None,
                    ),
                }
            }
            Expr::Fn(function) => {
                let parameters = function
                    .function
                    .params
                    .iter()
                    .map(|parameter| parameter.pat.clone())
                    .collect();
                let body = function
                    .function
                    .body
                    .as_ref()
                    .ok_or_else(|| anyhow!("runtime function expression has no body"))?;
                self.lower_nested_function(
                    parameters,
                    &body.stmts,
                    None,
                    function
                        .ident
                        .as_ref()
                        .map(|identifier| identifier.sym.to_string()),
                    function.function.is_async,
                    function.function.is_generator,
                    true,
                    None,
                )
            }
            Expr::This(_) => {
                let Some(_) = self.find_binding("*this*") else {
                    return Ok(self.constant(Constant::Undefined));
                };
                let binding = self.resolve_binding("*this*");
                Ok(self.load_binding(binding))
            }
            Expr::SuperProp(property) => self.lower_super_property(property),
            Expr::Class(class) => self.lower_class_expression(class),
            Expr::Call(call) => {
                if matches!(call.callee, Callee::Import(_)) {
                    if call.args.len() != 1 || call.args[0].spread.is_some() {
                        return Err(anyhow!(
                            "runtime dynamic import requires one non-spread specifier"
                        ));
                    }
                    let specifier = self.lower_expression(&call.args[0].expr)?;
                    let dst = self.register();
                    self.emit(Instruction::DynamicImport { dst, specifier });
                    return Ok(dst);
                }
                if matches!(call.callee, Callee::Super(_)) {
                    if self.find_binding("*super*").is_none() {
                        return Err(anyhow!("super() used outside a derived runtime class"));
                    }
                    if self.find_binding("*this*").is_none() {
                        return Err(anyhow!("super() used outside a runtime constructor"));
                    }
                    let parent = self.resolve_binding("*super*");
                    let this_binding = self.resolve_binding("*this*");
                    let parent = self.load_binding(parent);
                    let this_value = self.load_binding(this_binding);
                    let arguments = self.lower_arguments(&call.args, &[this_value])?;
                    let key = self.constant(Constant::String("__w3cos_super_ctor".into()));
                    let dst = self.register();
                    self.emit_method_call(dst, parent, key, arguments);
                    return Ok(dst);
                }
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::SuperProp(property) = callee.as_ref()
                {
                    if self.find_binding("*super*").is_none()
                        || self.find_binding("*this*").is_none()
                    {
                        return Err(anyhow!(
                            "super property call used outside a derived runtime class method"
                        ));
                    }
                    let parent_binding = self.resolve_binding("*super*");
                    let this_binding = self.resolve_binding("*this*");
                    let parent = self.load_binding(parent_binding);
                    let this_value = self.load_binding(this_binding);
                    let property = self.lower_super_property_key(property)?;
                    let arguments = self.lower_arguments(&call.args, &[this_value, property])?;
                    let bridge = if self.class_super_is_static == Some(true) {
                        "__w3cos_static_super_method"
                    } else {
                        "__w3cos_super_method"
                    };
                    let key = self.constant(Constant::String(bridge.into()));
                    let dst = self.register();
                    self.emit_method_call(dst, parent, key, arguments);
                    return Ok(dst);
                }
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Member(member) = callee.as_ref()
                    && matches!(member.prop, MemberProp::PrivateName(_))
                {
                    let (object, brand, name) = self.lower_private_member_parts(member)?;
                    let callee = self.register();
                    self.emit(Instruction::GetPrivate {
                        dst: callee,
                        object,
                        brand,
                        name,
                    });
                    let arguments = self.lower_arguments(&call.args, &[])?;
                    let dst = self.register();
                    self.emit_call(dst, callee, object, arguments);
                    return Ok(dst);
                }
                let member = match &call.callee {
                    Callee::Expr(callee) => match callee.as_ref() {
                        Expr::Member(member) => Some(self.lower_member_parts(member)?),
                        _ => None,
                    },
                    _ => return Err(anyhow!("unsupported runtime call target")),
                };
                let callee = if member.is_none() {
                    match &call.callee {
                        Callee::Expr(callee) => Some(self.lower_expression(callee)?),
                        _ => unreachable!("call target checked above"),
                    }
                } else {
                    None
                };
                let arguments = self.lower_arguments(&call.args, &[])?;
                let dst = self.register();
                if let Some((object, key)) = member {
                    self.emit_method_call(dst, object, key, arguments);
                } else {
                    let this_value = self.constant(Constant::Undefined);
                    self.emit_call(
                        dst,
                        callee.expect("non-member call has callee"),
                        this_value,
                        arguments,
                    );
                }
                Ok(dst)
            }
            Expr::New(expression) => {
                let constructor = self.lower_expression(&expression.callee)?;
                let arguments =
                    self.lower_arguments(expression.args.as_deref().unwrap_or_default(), &[])?;
                let dst = self.register();
                self.emit_construct(dst, constructor, arguments);
                Ok(dst)
            }
            Expr::MetaProp(meta) if meta.kind == swc_ecma_ast::MetaPropKind::ImportMeta => {
                let dst = self.register();
                self.emit(Instruction::ImportMeta { dst });
                Ok(dst)
            }
            Expr::Await(await_expression) => self.lower_await_expression(&await_expression.arg),
            Expr::Yield(yield_expression) => {
                if !self.is_generator {
                    return Err(anyhow!("yield is only valid inside a generator function"));
                }
                if yield_expression.delegate {
                    let expression = yield_expression.arg.as_deref().ok_or_else(|| {
                        anyhow!("runtime W3IR yield delegation requires an iterable")
                    })?;
                    let iterable = self.lower_expression(expression)?;
                    let key = self.constant(Constant::String(
                        if self.is_async {
                            "__w3cos_symbol_async_iterator"
                        } else {
                            "__w3cos_symbol_iterator"
                        }
                        .into(),
                    ));
                    let iterator = self.register();
                    self.emit(Instruction::CallMethod {
                        dst: iterator,
                        object: iterable,
                        key,
                        arguments: Vec::new(),
                    });
                    return self.emit_yield_delegate(iterator);
                }
                let value = if let Some(argument) = &yield_expression.arg {
                    self.lower_expression(argument)?
                } else {
                    self.constant(Constant::Undefined)
                };
                self.emit_yield(value)
            }
            Expr::Bin(binary)
                if binary.op == BinaryOp::In
                    && matches!(binary.left.as_ref(), Expr::PrivateName(_)) =>
            {
                self.find_binding("*class-brand*")
                    .ok_or_else(|| anyhow!("private brand check used outside its class"))?;
                let brand_binding = self.resolve_binding("*class-brand*");
                let brand = self.load_binding(brand_binding);
                let object = self.lower_expression(&binary.right)?;
                let dst = self.register();
                self.emit(Instruction::HasPrivate { dst, object, brand });
                Ok(dst)
            }
            Expr::Bin(binary)
                if matches!(
                    binary.op,
                    BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::NullishCoalescing
                ) =>
            {
                self.lower_logical_expression(binary)
            }
            Expr::Cond(conditional) => self.lower_conditional_expression(conditional),
            Expr::OptChain(optional) => self.lower_optional_chain(optional),
            Expr::Unary(unary) => self.lower_unary_expression(unary),
            Expr::Update(update) => self.lower_update_expression(update),
            Expr::Paren(parenthesized) => self.lower_expression(&parenthesized.expr),
            Expr::Seq(sequence) => {
                let mut result = None;
                for expression in &sequence.exprs {
                    result = Some(self.lower_expression(expression)?);
                }
                result.ok_or_else(|| anyhow!("runtime W3IR sequence expression is empty"))
            }
            Expr::Bin(binary) if binary.op == BinaryOp::Add => {
                let lhs = self.lower_expression(&binary.left)?;
                let rhs = self.lower_expression(&binary.right)?;
                let dst = self.register();
                self.emit(Instruction::Add { dst, lhs, rhs });
                Ok(dst)
            }
            Expr::Bin(binary) => {
                let operator = match binary.op {
                    BinaryOp::Sub => BinaryOperator::Subtract,
                    BinaryOp::Mul => BinaryOperator::Multiply,
                    BinaryOp::Div => BinaryOperator::Divide,
                    BinaryOp::Mod => BinaryOperator::Remainder,
                    BinaryOp::Exp => BinaryOperator::Exponentiate,
                    BinaryOp::EqEq => BinaryOperator::AbstractEqual,
                    BinaryOp::NotEq => BinaryOperator::AbstractNotEqual,
                    BinaryOp::EqEqEq => BinaryOperator::StrictEqual,
                    BinaryOp::NotEqEq => BinaryOperator::StrictNotEqual,
                    BinaryOp::Lt => BinaryOperator::LessThan,
                    BinaryOp::LtEq => BinaryOperator::LessThanOrEqual,
                    BinaryOp::Gt => BinaryOperator::GreaterThan,
                    BinaryOp::GtEq => BinaryOperator::GreaterThanOrEqual,
                    BinaryOp::LShift => BinaryOperator::LeftShift,
                    BinaryOp::RShift => BinaryOperator::SignedRightShift,
                    BinaryOp::ZeroFillRShift => BinaryOperator::UnsignedRightShift,
                    BinaryOp::BitOr => BinaryOperator::BitwiseOr,
                    BinaryOp::BitXor => BinaryOperator::BitwiseXor,
                    BinaryOp::BitAnd => BinaryOperator::BitwiseAnd,
                    BinaryOp::InstanceOf => BinaryOperator::InstanceOf,
                    BinaryOp::In => BinaryOperator::In,
                    _ => {
                        return Err(anyhow!(
                            "runtime W3IR does not yet support binary operator {:?}",
                            binary.op
                        ));
                    }
                };
                let lhs = self.lower_expression(&binary.left)?;
                let rhs = self.lower_expression(&binary.right)?;
                let dst = self.register();
                self.emit(Instruction::Binary {
                    dst,
                    operator,
                    lhs,
                    rhs,
                });
                Ok(dst)
            }
            Expr::Assign(assignment)
                if matches!(
                    assignment.op,
                    AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::NullishAssign
                ) =>
            {
                self.lower_logical_assignment(assignment)
            }
            Expr::Assign(assignment)
                if assignment.op == AssignOp::Assign
                    && matches!(&assignment.left, AssignTarget::Pat(_)) =>
            {
                self.lower_destructuring_assignment(assignment)
            }
            Expr::Assign(assignment)
                if assignment.op != AssignOp::Assign
                    && assignment.op.to_update().is_some_and(|operator| {
                        matches!(
                            operator,
                            BinaryOp::Add
                                | BinaryOp::Sub
                                | BinaryOp::Mul
                                | BinaryOp::Div
                                | BinaryOp::Mod
                                | BinaryOp::Exp
                                | BinaryOp::LShift
                                | BinaryOp::RShift
                                | BinaryOp::ZeroFillRShift
                                | BinaryOp::BitOr
                                | BinaryOp::BitXor
                                | BinaryOp::BitAnd
                        )
                    }) =>
            {
                self.lower_compound_assignment(assignment)
            }
            Expr::Assign(assignment) if assignment.op == AssignOp::Assign => {
                self.lower_simple_assignment(assignment)
            }
            unsupported => Err(anyhow!(
                "runtime W3IR lowering does not yet support this expression: {unsupported:?}"
            )),
        }
    }

    fn lower_template_literal(&mut self, template: &swc_ecma_ast::Tpl) -> Result<Register> {
        if template.quasis.len() != template.exprs.len() + 1 {
            return Err(anyhow!(
                "runtime W3IR template literal has inconsistent quasi/expression counts"
            ));
        }
        let first = template
            .quasis
            .first()
            .and_then(|quasi| quasi.cooked.as_ref())
            .ok_or_else(|| anyhow!("runtime W3IR template literal has an invalid escape"))?;
        let mut value = self.constant(Constant::String(atom_to_string(first)));
        for (expression, quasi) in template.exprs.iter().zip(template.quasis.iter().skip(1)) {
            let expression = self.lower_expression(expression)?;
            let interpolated = self.register();
            self.emit(Instruction::Add {
                dst: interpolated,
                lhs: value,
                rhs: expression,
            });
            value = interpolated;

            let cooked = quasi
                .cooked
                .as_ref()
                .ok_or_else(|| anyhow!("runtime W3IR template literal has an invalid escape"))?;
            let cooked = atom_to_string(cooked);
            if !cooked.is_empty() {
                let suffix = self.constant(Constant::String(cooked));
                let concatenated = self.register();
                self.emit(Instruction::Add {
                    dst: concatenated,
                    lhs: value,
                    rhs: suffix,
                });
                value = concatenated;
            }
        }
        Ok(value)
    }

    fn lower_optional_chain(
        &mut self,
        expression: &swc_ecma_ast::OptChainExpr,
    ) -> Result<Register> {
        match expression.base.as_ref() {
            OptChainBase::Member(member) => {
                let object = self.lower_expression(&member.obj)?;
                let null = self.constant(Constant::Null);
                let is_nullish = self.register();
                self.emit(Instruction::Binary {
                    dst: is_nullish,
                    operator: BinaryOperator::AbstractEqual,
                    lhs: object,
                    rhs: null,
                });
                self.lower_branch_value(
                    is_nullish,
                    |builder| Ok(builder.constant(Constant::Undefined)),
                    |builder| {
                        let dst = builder.register();
                        if let MemberProp::PrivateName(private) = &member.prop {
                            builder.find_binding("*class-brand*").ok_or_else(|| {
                                anyhow!("private optional member used outside its class")
                            })?;
                            let brand_binding = builder.resolve_binding("*class-brand*");
                            let brand = builder.load_binding(brand_binding);
                            let name = builder.constant(Constant::String(private.name.to_string()));
                            builder.emit(Instruction::GetPrivate {
                                dst,
                                object,
                                brand,
                                name,
                            });
                        } else {
                            let key = builder.lower_member_key(&member.prop)?;
                            builder.emit(Instruction::GetProperty { dst, object, key });
                        }
                        Ok(dst)
                    },
                )
            }
            OptChainBase::Call(call) => {
                if let Some(member) = optional_chain_member(&call.callee) {
                    let receiver = self.lower_expression(&member.obj)?;
                    let null = self.constant(Constant::Null);
                    let receiver_nullish = self.register();
                    self.emit(Instruction::Binary {
                        dst: receiver_nullish,
                        operator: BinaryOperator::AbstractEqual,
                        lhs: receiver,
                        rhs: null,
                    });
                    return self.lower_branch_value(
                        receiver_nullish,
                        |builder| Ok(builder.constant(Constant::Undefined)),
                        |builder| {
                            let callee = builder.register();
                            if let MemberProp::PrivateName(private) = &member.prop {
                                builder.find_binding("*class-brand*").ok_or_else(|| {
                                    anyhow!("private optional call used outside its class")
                                })?;
                                let brand_binding = builder.resolve_binding("*class-brand*");
                                let brand = builder.load_binding(brand_binding);
                                let name =
                                    builder.constant(Constant::String(private.name.to_string()));
                                builder.emit(Instruction::GetPrivate {
                                    dst: callee,
                                    object: receiver,
                                    brand,
                                    name,
                                });
                            } else {
                                let key = builder.lower_member_key(&member.prop)?;
                                builder.emit(Instruction::GetProperty {
                                    dst: callee,
                                    object: receiver,
                                    key,
                                });
                            }
                            let null = builder.constant(Constant::Null);
                            let callee_nullish = builder.register();
                            builder.emit(Instruction::Binary {
                                dst: callee_nullish,
                                operator: BinaryOperator::AbstractEqual,
                                lhs: callee,
                                rhs: null,
                            });
                            builder.lower_branch_value(
                                callee_nullish,
                                |builder| Ok(builder.constant(Constant::Undefined)),
                                |builder| {
                                    let arguments = builder.lower_arguments(&call.args, &[])?;
                                    let dst = builder.register();
                                    builder.emit_call(dst, callee, receiver, arguments);
                                    Ok(dst)
                                },
                            )
                        },
                    );
                }

                let callee = self.lower_expression(&call.callee)?;
                let null = self.constant(Constant::Null);
                let is_nullish = self.register();
                self.emit(Instruction::Binary {
                    dst: is_nullish,
                    operator: BinaryOperator::AbstractEqual,
                    lhs: callee,
                    rhs: null,
                });
                self.lower_branch_value(
                    is_nullish,
                    |builder| Ok(builder.constant(Constant::Undefined)),
                    |builder| {
                        let arguments = builder.lower_arguments(&call.args, &[])?;
                        let this_value = builder.constant(Constant::Undefined);
                        let dst = builder.register();
                        builder.emit_call(dst, callee, this_value, arguments);
                        Ok(dst)
                    },
                )
            }
        }
    }

    fn lower_jsx_element(&mut self, element: &swc_ecma_ast::JSXElement) -> Result<Register> {
        let element_type = self.lower_jsx_name(&element.opening.name)?;
        let props = self.register();
        self.emit(Instruction::CreateObject {
            dst: props,
            properties: Vec::new(),
        });
        for attribute in &element.opening.attrs {
            let attribute = match attribute {
                swc_ecma_ast::JSXAttrOrSpread::SpreadElement(spread) => {
                    let source = self.lower_expression(&spread.expr)?;
                    self.emit(Instruction::CopyDataProperties {
                        target: props,
                        source,
                    });
                    continue;
                }
                swc_ecma_ast::JSXAttrOrSpread::JSXAttr(attribute) => attribute,
            };
            let key = match &attribute.name {
                swc_ecma_ast::JSXAttrName::Ident(identifier) => identifier.sym.to_string(),
                swc_ecma_ast::JSXAttrName::JSXNamespacedName(name) => {
                    format!("{}:{}", name.ns.sym, name.name.sym)
                }
            };
            let key = self.constant(Constant::String(key));
            let value = match attribute.value.as_ref() {
                None => self.constant(Constant::Bool(true)),
                Some(swc_ecma_ast::JSXAttrValue::Str(value)) => {
                    self.constant(Constant::String(atom_to_string(&value.value)))
                }
                Some(swc_ecma_ast::JSXAttrValue::JSXExprContainer(container)) => {
                    match &container.expr {
                        swc_ecma_ast::JSXExpr::Expr(expression) => {
                            self.lower_expression(expression)?
                        }
                        swc_ecma_ast::JSXExpr::JSXEmptyExpr(_) => {
                            self.constant(Constant::Undefined)
                        }
                    }
                }
                Some(swc_ecma_ast::JSXAttrValue::JSXElement(element)) => {
                    self.lower_jsx_element(element)?
                }
                Some(swc_ecma_ast::JSXAttrValue::JSXFragment(fragment)) => {
                    self.lower_jsx_children(&fragment.children)?
                }
            };
            self.emit(Instruction::DefineField {
                object: props,
                key,
                value,
            });
        }

        let children = self.lower_jsx_child_values(&element.children)?;
        if !children.is_empty() {
            let children_value = self.register();
            self.emit(Instruction::CreateArray {
                dst: children_value,
                elements: children,
            });
            let key = self.constant(Constant::String("children".into()));
            self.emit(Instruction::DefineField {
                object: props,
                key,
                value: children_value,
            });
        }

        let type_key = self.constant(Constant::String("type".into()));
        let props_key = self.constant(Constant::String("props".into()));
        let dst = self.register();
        self.emit(Instruction::CreateObject {
            dst,
            properties: vec![(type_key, element_type), (props_key, props)],
        });
        Ok(dst)
    }

    fn lower_jsx_name(&mut self, name: &swc_ecma_ast::JSXElementName) -> Result<Register> {
        match name {
            swc_ecma_ast::JSXElementName::Ident(identifier) => {
                let name = identifier.sym.to_string();
                if name.chars().next().is_some_and(char::is_lowercase) {
                    Ok(self.constant(Constant::String(name)))
                } else {
                    let binding = self.resolve_binding(&name);
                    Ok(self.load_binding(binding))
                }
            }
            swc_ecma_ast::JSXElementName::JSXMemberExpr(member) => {
                self.lower_jsx_member_name(member)
            }
            swc_ecma_ast::JSXElementName::JSXNamespacedName(name) => Ok(self.constant(
                Constant::String(format!("{}:{}", name.ns.sym, name.name.sym)),
            )),
        }
    }

    fn lower_jsx_member_name(&mut self, member: &swc_ecma_ast::JSXMemberExpr) -> Result<Register> {
        let object = match &member.obj {
            swc_ecma_ast::JSXObject::Ident(identifier) => {
                let binding = self.resolve_binding(identifier.sym.as_ref());
                self.load_binding(binding)
            }
            swc_ecma_ast::JSXObject::JSXMemberExpr(member) => self.lower_jsx_member_name(member)?,
        };
        let key = self.constant(Constant::String(member.prop.sym.to_string()));
        let dst = self.register();
        self.emit(Instruction::GetProperty { dst, object, key });
        Ok(dst)
    }

    fn lower_jsx_child_values(
        &mut self,
        children: &[swc_ecma_ast::JSXElementChild],
    ) -> Result<Vec<Register>> {
        let mut values = Vec::new();
        for child in children {
            let value = match child {
                swc_ecma_ast::JSXElementChild::JSXText(text) => {
                    let text = text.value.split_whitespace().collect::<Vec<_>>().join(" ");
                    if text.is_empty() {
                        continue;
                    }
                    self.constant(Constant::String(text))
                }
                swc_ecma_ast::JSXElementChild::JSXExprContainer(container) => {
                    match &container.expr {
                        swc_ecma_ast::JSXExpr::Expr(expression) => {
                            self.lower_expression(expression)?
                        }
                        swc_ecma_ast::JSXExpr::JSXEmptyExpr(_) => continue,
                    }
                }
                swc_ecma_ast::JSXElementChild::JSXSpreadChild(spread) => {
                    self.lower_expression(&spread.expr)?
                }
                swc_ecma_ast::JSXElementChild::JSXElement(element) => {
                    self.lower_jsx_element(element)?
                }
                swc_ecma_ast::JSXElementChild::JSXFragment(fragment) => {
                    self.lower_jsx_children(&fragment.children)?
                }
            };
            values.push(value);
        }
        Ok(values)
    }

    fn lower_jsx_children(
        &mut self,
        children: &[swc_ecma_ast::JSXElementChild],
    ) -> Result<Register> {
        let elements = self.lower_jsx_child_values(children)?;
        let dst = self.register();
        self.emit(Instruction::CreateArray { dst, elements });
        Ok(dst)
    }

    fn lower_class_expression(&mut self, expression: &swc_ecma_ast::ClassExpr) -> Result<Register> {
        let Some(identifier) = &expression.ident else {
            return self.lower_class_value(&expression.class, None);
        };
        self.scopes.push(HashMap::new());
        let result = (|| {
            let name = identifier.sym.to_string();
            let binding = self.declare_class_binding(&name)?;
            let value = self.lower_class_value(&expression.class, Some(name))?;
            self.emit(Instruction::InitializeBinding { binding, value });
            Ok(value)
        })();
        self.scopes.pop();
        result
    }

    fn lower_class_value(&mut self, class: &Class, name: Option<String>) -> Result<Register> {
        self.scopes.push(HashMap::new());
        let result = self.lower_class_value_in_scope(class, name);
        self.scopes.pop();
        result
    }

    fn lower_class_value_in_scope(
        &mut self,
        class: &Class,
        name: Option<String>,
    ) -> Result<Register> {
        // Private-name operations capture this lexical cell. It is
        // initialized after CreateClass returns, before any static
        // initializer runs or an instance can be constructed.
        let class_brand_binding =
            self.declare_current_binding("*class-brand*", BindingKind::Const, false)?;
        self.lower_class_definition_values(class, name.as_deref())?;
        let super_class = class
            .super_class
            .as_ref()
            .map(|expression| self.lower_expression(expression))
            .transpose()?;
        let super_binding = if let Some(super_class) = super_class {
            let binding = self.declare_current_binding("*super*", BindingKind::Const, false)?;
            self.emit(Instruction::InitializeBinding {
                binding,
                value: super_class,
            });
            Some(super_class)
        } else {
            None
        };

        // Property keys are evaluated once when the class is defined. The
        // initializer closures capture those keys while evaluating field
        // values once per instance (or once on the class for static fields).
        let mut instance_fields = Vec::new();
        let mut static_initializers = Vec::new();
        let mut public_method_keys = HashMap::new();
        let mut static_index = 0usize;
        let mut computed_method_index = 0usize;
        for (member_index, member) in class.body.iter().enumerate() {
            match member {
                ClassMember::ClassProp(field) => {
                    let key = self.lower_property_name(&field.key)?;
                    let key_name = format!("*class-field-key-{}*", self.next_binding);
                    let key_binding =
                        self.declare_current_binding(&key_name, BindingKind::Const, false)?;
                    self.emit(Instruction::InitializeBinding {
                        binding: key_binding,
                        value: key,
                    });
                    let prepared = (Some(key_name), String::new(), field.value.as_deref());
                    if field.is_static {
                        let initializer = self
                            .lower_class_field_initializer(
                                &[prepared],
                                name.clone()
                                    .map(|name| format!("{name}.<static_{static_index}>")),
                                true,
                            )?
                            .expect("one static field always creates an initializer");
                        static_initializers.push(initializer);
                        static_index += 1;
                    } else {
                        instance_fields.push(prepared);
                    }
                }
                ClassMember::PrivateProp(field) => {
                    let prepared = (None, field.key.name.to_string(), field.value.as_deref());
                    if field.is_static {
                        let initializer = self
                            .lower_class_field_initializer(
                                &[prepared],
                                name.clone()
                                    .map(|name| format!("{name}.<static_{static_index}>")),
                                true,
                            )?
                            .expect("one static private field always creates an initializer");
                        static_initializers.push(initializer);
                        static_index += 1;
                    } else {
                        instance_fields.push(prepared);
                    }
                }
                ClassMember::StaticBlock(block) => {
                    let initializer = self.lower_nested_function(
                        Vec::new(),
                        &block.body.stmts,
                        None,
                        name.clone()
                            .map(|name| format!("{name}.<static_{static_index}>")),
                        false,
                        false,
                        true,
                        Some(true),
                    )?;
                    static_initializers.push(initializer);
                    static_index += 1;
                }
                ClassMember::Method(method) => {
                    let prefix = match method.kind {
                        MethodKind::Method => "",
                        MethodKind::Getter => "__w3cos_getter_",
                        MethodKind::Setter => "__w3cos_setter_",
                    };
                    let key = self.lower_prefixed_property_name(prefix, &method.key)?;
                    public_method_keys.insert(member_index, key);
                }
                _ => {}
            }
        }
        let instance_initializer = self.lower_class_field_initializer(
            &instance_fields,
            name.clone().map(|name| format!("{name}.<instance_fields>")),
            false,
        )?;
        let mut constructor = None;
        for member in &class.body {
            if let ClassMember::Constructor(candidate) = member {
                if constructor.is_some() {
                    return Err(anyhow!(
                        "runtime W3IR class has multiple constructor definitions"
                    ));
                }
                let mut parameters = Vec::with_capacity(candidate.params.len());
                let mut parameter_properties = Vec::new();
                for parameter in &candidate.params {
                    match parameter {
                        ParamOrTsParamProp::Param(parameter) => {
                            parameters.push(parameter.pat.clone());
                        }
                        ParamOrTsParamProp::TsParamProp(property) => {
                            let (pattern, identifier) = parameter_property_parts(property)?;
                            parameters.push(pattern);
                            parameter_properties.push(parameter_property_assignment(&identifier));
                        }
                    }
                }
                let mut statements = candidate
                    .body
                    .as_ref()
                    .map(|body| body.stmts.clone())
                    .unwrap_or_default();
                if !parameter_properties.is_empty() {
                    if class.super_class.is_some() {
                        let super_index = statements
                            .iter()
                            .position(direct_super_call)
                            .ok_or_else(|| {
                                anyhow!(
                                    "runtime W3IR derived TypeScript parameter properties require a direct super() statement"
                                )
                            })?;
                        statements.splice(super_index + 1..super_index + 1, parameter_properties);
                    } else {
                        parameter_properties.extend(statements);
                        statements = parameter_properties;
                    }
                }
                constructor = Some(self.lower_nested_function(
                    parameters,
                    &statements,
                    None,
                    name.clone().map(|name| format!("{name}.constructor")),
                    false,
                    false,
                    true,
                    Some(false),
                )?);
            }
        }
        let constructor = constructor.unwrap_or_else(|| self.constant(Constant::Undefined));
        let class_value = self.register();
        self.emit(Instruction::CreateClass {
            dst: class_value,
            constructor,
            super_class: super_binding,
            initializer: instance_initializer,
        });
        self.emit(Instruction::InitializeBinding {
            binding: class_brand_binding,
            value: class_value,
        });
        let prototype_key = self.constant(Constant::String("prototype".into()));
        let prototype = self.register();
        self.emit(Instruction::GetProperty {
            dst: prototype,
            object: class_value,
            key: prototype_key,
        });

        for (member_index, member) in class.body.iter().enumerate() {
            match member {
                ClassMember::Constructor(_)
                | ClassMember::ClassProp(_)
                | ClassMember::PrivateProp(_)
                | ClassMember::StaticBlock(_)
                | ClassMember::Empty(_) => {}
                ClassMember::Method(method) => {
                    let body = method
                        .function
                        .body
                        .as_ref()
                        .ok_or_else(|| anyhow!("runtime W3IR class method has no body"))?;
                    let parameters = method
                        .function
                        .params
                        .iter()
                        .map(|parameter| parameter.pat.clone())
                        .collect();
                    let method_name = match &method.key {
                        PropName::Ident(identifier) => Some(identifier.sym.to_string()),
                        PropName::Str(value) => {
                            Some(format!("{:?}", value.value).trim_matches('"').to_string())
                        }
                        _ => {
                            computed_method_index += 1;
                            Some(format!("<computed_{computed_method_index}>"))
                        }
                    }
                    .map(|method_name| match method.kind {
                        MethodKind::Method => method_name,
                        MethodKind::Getter => format!("get {method_name}"),
                        MethodKind::Setter => format!("set {method_name}"),
                    });
                    let value = self.lower_nested_function(
                        parameters,
                        &body.stmts,
                        None,
                        method_name.map(|method_name| {
                            name.as_deref()
                                .map(|name| {
                                    if method.is_static {
                                        format!("{name}.static {method_name}")
                                    } else {
                                        format!("{name}.{method_name}")
                                    }
                                })
                                .unwrap_or(method_name)
                        }),
                        method.function.is_async,
                        method.function.is_generator,
                        true,
                        Some(method.is_static),
                    )?;
                    let key = public_method_keys
                        .get(&member_index)
                        .copied()
                        .ok_or_else(|| anyhow!("runtime W3IR class method key was not prepared"))?;
                    self.emit(Instruction::SetProperty {
                        object: if method.is_static {
                            class_value
                        } else {
                            prototype
                        },
                        key,
                        value,
                    });
                }
                ClassMember::PrivateMethod(method) => {
                    let body = method
                        .function
                        .body
                        .as_ref()
                        .ok_or_else(|| anyhow!("runtime W3IR private method has no body"))?;
                    let parameters = method
                        .function
                        .params
                        .iter()
                        .map(|parameter| parameter.pat.clone())
                        .collect();
                    let private_name = method.key.name.to_string();
                    let private_identity = match method.kind {
                        MethodKind::Method => format!("#{private_name}"),
                        MethodKind::Getter => format!("get #{private_name}"),
                        MethodKind::Setter => format!("set #{private_name}"),
                    };
                    let value = self.lower_nested_function(
                        parameters,
                        &body.stmts,
                        None,
                        name.as_deref().map(|name| {
                            if method.is_static {
                                format!("{name}.static {private_identity}")
                            } else {
                                format!("{name}.{private_identity}")
                            }
                        }),
                        method.function.is_async,
                        method.function.is_generator,
                        true,
                        Some(method.is_static),
                    )?;
                    let key = self.constant(Constant::String(private_name));
                    match method.kind {
                        MethodKind::Method => {
                            self.emit(Instruction::DefinePrivateMethod {
                                brand: class_value,
                                name: key,
                                value,
                            });
                        }
                        MethodKind::Getter => {
                            self.emit(Instruction::DefinePrivateAccessor {
                                brand: class_value,
                                name: key,
                                getter: Some(value),
                                setter: None,
                            });
                        }
                        MethodKind::Setter => {
                            self.emit(Instruction::DefinePrivateAccessor {
                                brand: class_value,
                                name: key,
                                getter: None,
                                setter: Some(value),
                            });
                        }
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "runtime W3IR private class members are not yet supported"
                    ));
                }
            }
        }
        for initializer in static_initializers {
            let dst = self.register();
            self.emit(Instruction::Call {
                dst,
                callee: initializer,
                this_value: class_value,
                arguments: Vec::new(),
            });
        }
        Ok(class_value)
    }

    /// Preserve the source-ordered class-definition expressions as a
    /// backend-neutral W3IR function. W3VM evaluates the same expressions
    /// directly while executing `CreateClass`; the native backend consumes
    /// this synthetic function instead of returning to direct AST lowering for
    /// `extends` and computed/public keys.
    ///
    /// The returned array contains, in order: the optional `extends` value,
    /// every public field key, and every computed public method/accessor key.
    /// Getter/setter keys include the same internal prefix used by the class
    /// property ABI.
    fn lower_class_definition_values(&mut self, class: &Class, name: Option<&str>) -> Result<()> {
        let Some(name) = name else {
            return Ok(());
        };
        let has_values = class.super_class.is_some()
            || class.body.iter().any(|member| match member {
                ClassMember::ClassProp(_) => true,
                ClassMember::Method(method) => matches!(method.key, PropName::Computed(_)),
                _ => false,
            });
        if !has_values {
            return Ok(());
        }

        let function_id = FunctionId(self.next_function);
        self.next_function += 1;
        let mut child = Builder::nested(
            self.visible_bindings(),
            self.globals.clone(),
            self.next_binding,
            self.next_function,
            false,
            false,
            false,
        );
        let mut values = Vec::new();
        if let Some(super_class) = &class.super_class {
            values.push(child.lower_expression(super_class)?);
        }
        for member in &class.body {
            match member {
                ClassMember::ClassProp(field) => {
                    values.push(child.lower_property_name(&field.key)?);
                }
                ClassMember::Method(method) if matches!(method.key, PropName::Computed(_)) => {
                    let prefix = match method.kind {
                        MethodKind::Method => "",
                        MethodKind::Getter => "__w3cos_getter_",
                        MethodKind::Setter => "__w3cos_setter_",
                    };
                    values.push(child.lower_prefixed_property_name(prefix, &method.key)?);
                }
                _ => {}
            }
        }
        let result = child.register();
        child.emit(Instruction::CreateArray {
            dst: result,
            elements: values,
        });
        child.terminate(Instruction::Return { value: result });
        child.seal_current_block();

        self.merge_child_scope(&child);
        let function = Function {
            id: function_id,
            name: Some(format!("{name}.<definition_values>")),
            parameters: child.parameters,
            rest_parameter: child.rest_parameter,
            arguments_binding: child.arguments_binding,
            bindings: child.bindings,
            captures: child.capture_order.clone(),
            this_binding: None,
            registers: child.next_register,
            entry: BlockId(0),
            blocks: child.blocks.clone(),
            exception_regions: child.exception_regions,
            suspension_points: child.suspension_points,
            generator_suspension_points: Vec::new(),
            is_async: false,
            is_generator: false,
            source_span: None,
        };
        self.functions.extend(child.functions);
        self.functions.push(function);
        Ok(())
    }

    fn lower_class_field_initializer(
        &mut self,
        fields: &[(Option<String>, String, Option<&Expr>)],
        name: Option<String>,
        is_static: bool,
    ) -> Result<Option<Register>> {
        if fields.is_empty() {
            return Ok(None);
        }
        let function_id = FunctionId(self.next_function);
        self.next_function += 1;
        let mut child = Builder::nested(
            self.visible_bindings(),
            self.globals.clone(),
            self.next_binding,
            self.next_function,
            false,
            false,
            false,
        );
        child.class_super_is_static = Some(is_static);
        let this_binding = child.declare_this_binding()?;
        let this_value = child.load_binding(this_binding);
        for (key_name, private_name, expression) in fields {
            let value = if let Some(expression) = expression {
                child.lower_expression(expression)?
            } else {
                child.constant(Constant::Undefined)
            };
            if let Some(key_name) = key_name {
                let key_binding = child.resolve_binding(key_name);
                let key = child.load_binding(key_binding);
                child.emit(Instruction::DefineField {
                    object: this_value,
                    key,
                    value,
                });
            } else {
                let brand_binding = child.resolve_binding("*class-brand*");
                let brand = child.load_binding(brand_binding);
                let name = child.constant(Constant::String(private_name.clone()));
                child.emit(Instruction::DefinePrivate {
                    object: this_value,
                    brand,
                    name,
                    value,
                });
            }
        }
        let undefined = child.constant(Constant::Undefined);
        child.terminate(Instruction::Return { value: undefined });
        child.seal_current_block();

        self.merge_child_scope(&child);
        let captures = child.capture_order.clone();
        let function = Function {
            id: function_id,
            name,
            parameters: child.parameters,
            rest_parameter: child.rest_parameter,
            arguments_binding: child.arguments_binding,
            bindings: child.bindings,
            captures: captures.clone(),
            this_binding: Some(this_binding),
            registers: child.next_register,
            entry: BlockId(0),
            blocks: child.blocks.clone(),
            exception_regions: child.exception_regions,
            suspension_points: child.suspension_points,
            generator_suspension_points: Vec::new(),
            is_async: false,
            is_generator: false,
            source_span: None,
        };
        self.functions.extend(child.functions);
        self.functions.push(function);
        let dst = self.register();
        self.emit(Instruction::CreateClosure {
            dst,
            function: function_id,
            captures,
        });
        Ok(Some(dst))
    }

    fn lower_nested_function(
        &mut self,
        parameters: Vec<Pat>,
        statements: &[Stmt],
        expression_body: Option<&Expr>,
        name: Option<String>,
        is_async: bool,
        is_generator: bool,
        bind_this: bool,
        class_super_is_static: Option<bool>,
    ) -> Result<Register> {
        let function_id = FunctionId(self.next_function);
        self.next_function += 1;
        let mut child = Builder::nested(
            self.visible_bindings(),
            self.globals.clone(),
            self.next_binding,
            self.next_function,
            is_async,
            is_generator,
            self.annex_b_function_declarations
                && !statements_have_use_strict_directive(statements.iter()),
        );
        child.class_super_is_static = class_super_is_static
            .or_else(|| (!bind_this).then_some(self.class_super_is_static).flatten());
        let this_binding = if bind_this {
            Some(child.declare_this_binding()?)
        } else {
            None
        };
        let mut parameter_sources = Vec::with_capacity(parameters.len());
        for (index, parameter) in parameters.iter().enumerate() {
            if matches!(parameter, Pat::Rest(_)) && index + 1 != parameters.len() {
                return Err(anyhow!(
                    "runtime W3IR rest parameter must be the final parameter"
                ));
            }
            parameter_sources.push(child.declare_parameter(parameter, index)?);
        }
        if bind_this {
            child.declare_arguments_binding()?;
        }
        for statement in statements {
            child.predeclare_function_binding(statement)?;
        }
        for statement in statements {
            child.predeclare_annex_b_branch_functions(statement)?;
        }
        for statement in statements {
            child.predeclare_var_bindings(statement)?;
        }
        for statement in statements {
            child.predeclare_lexical_binding(statement)?;
        }
        for (parameter, source) in parameters.iter().zip(parameter_sources) {
            child.initialize_parameter(parameter, source)?;
        }
        for statement in statements {
            child.initialize_function_declaration(statement)?;
        }
        for statement in statements {
            child.lower_statement(statement)?;
        }
        if !child.terminated {
            let value = if let Some(expression) = expression_body {
                child.lower_expression(expression)?
            } else {
                child.constant(Constant::Undefined)
            };
            child.terminate(Instruction::Return { value });
        }
        child.seal_current_block();

        self.merge_child_scope(&child);
        let captures = child.capture_order.clone();
        let function = Function {
            id: function_id,
            name,
            parameters: child.parameters,
            rest_parameter: child.rest_parameter,
            arguments_binding: child.arguments_binding,
            bindings: child.bindings,
            captures: captures.clone(),
            this_binding,
            registers: child.next_register,
            entry: BlockId(0),
            blocks: child.blocks.clone(),
            exception_regions: child.exception_regions,
            suspension_points: child.suspension_points,
            generator_suspension_points: child.generator_suspension_points,
            is_async: child.is_async,
            is_generator: child.is_generator,
            source_span: None,
        };
        self.functions.extend(child.functions);
        self.functions.push(function);
        let dst = self.register();
        self.emit(Instruction::CreateClosure {
            dst,
            function: function_id,
            captures,
        });
        Ok(dst)
    }

    fn lower_await_expression(&mut self, expression: &Expr) -> Result<Register> {
        if !self.allows_await {
            return Err(anyhow!(
                "await is only supported inside async functions or modules"
            ));
        }
        let value = self.lower_expression(expression)?;
        self.emit_await(value)
    }

    fn emit_await(&mut self, value: Register) -> Result<Register> {
        if !self.allows_await {
            return Err(anyhow!(
                "await is only supported inside async functions or modules"
            ));
        }
        self.is_async = true;
        let dst = self.register();
        let suspension = SuspensionId(self.next_suspension);
        self.next_suspension += 1;
        let await_block = self.current_block;
        let resume_block = self.allocate_block();
        let reject_block = self.allocate_block();
        self.emit(Instruction::Await {
            dst,
            value,
            suspension,
        });
        self.terminate(Instruction::Jump {
            target: resume_block,
        });
        self.seal_current_block();

        self.start_block(reject_block);
        self.terminate(Instruction::Throw { value: dst });
        self.seal_current_block();
        self.start_block(resume_block);
        self.suspension_points.push(SuspensionPoint {
            id: suspension,
            await_block,
            resume_block,
            reject_block,
            live_registers: (0..dst.0).map(Register).collect(),
        });
        Ok(dst)
    }

    fn emit_yield(&mut self, value: Register) -> Result<Register> {
        if !self.is_generator {
            return Err(anyhow!("yield is only valid inside a generator function"));
        }
        let dst = self.register();
        let suspension = SuspensionId(self.next_suspension);
        self.next_suspension += 1;
        let yield_block = self.current_block;
        let resume_block = self.allocate_block();
        let throw_block = self.allocate_block();
        let return_block = self.allocate_block();

        self.emit(Instruction::Yield {
            dst,
            value,
            suspension,
        });
        self.terminate(Instruction::Jump {
            target: resume_block,
        });
        self.seal_current_block();

        self.start_block(throw_block);
        self.terminate(Instruction::Throw { value: dst });
        self.seal_current_block();

        self.start_block(return_block);
        self.lower_return_value(dst)?;
        self.seal_current_block();

        self.start_block(resume_block);
        self.generator_suspension_points
            .push(GeneratorSuspensionPoint {
                id: suspension,
                yield_block,
                result: dst,
                resume_block,
                throw_block,
                return_block,
                live_registers: (0..dst.0).map(Register).collect(),
            });
        Ok(dst)
    }

    fn emit_yield_delegate(&mut self, iterator: Register) -> Result<Register> {
        let dst = self.register();
        let suspension = SuspensionId(self.next_suspension);
        self.next_suspension += 1;
        let yield_block = self.current_block;
        let resume_block = self.allocate_block();
        let throw_block = self.allocate_block();
        let return_block = self.allocate_block();

        self.emit(Instruction::YieldDelegate {
            dst,
            iterator,
            suspension,
        });
        self.terminate(Instruction::Jump {
            target: resume_block,
        });
        self.seal_current_block();

        self.start_block(throw_block);
        self.terminate(Instruction::Throw { value: dst });
        self.seal_current_block();

        self.start_block(return_block);
        self.lower_return_value(dst)?;
        self.seal_current_block();

        self.start_block(resume_block);
        self.generator_suspension_points
            .push(GeneratorSuspensionPoint {
                id: suspension,
                yield_block,
                result: dst,
                resume_block,
                throw_block,
                return_block,
                live_registers: (0..dst.0).map(Register).collect(),
            });
        Ok(dst)
    }

    fn merge_child_scope(&mut self, child: &Builder) {
        self.next_binding = child.next_binding;
        self.next_function = child.next_function;
        for binding in &child.new_globals {
            if self.globals.contains_key(&binding.name) {
                continue;
            }
            self.globals.insert(binding.name.clone(), binding.id);
            if self.is_entry {
                self.bindings.push(binding.clone());
            } else {
                self.new_globals.push(binding.clone());
                self.capture(&binding.name, binding.id);
            }
        }
        for import in &child.imports {
            if !self
                .imports
                .iter()
                .any(|existing| existing.local == import.local)
            {
                self.imports.push(import.clone());
            }
        }
        for (name, binding) in &child.captures {
            let is_local = self.bindings.iter().any(|local| local.id == *binding);
            if !is_local && !self.is_entry {
                self.capture(name, *binding);
            }
        }
    }

    fn lower_property_name(&mut self, name: &PropName) -> Result<Register> {
        match name {
            PropName::Ident(identifier) => {
                Ok(self.constant(Constant::String(identifier.sym.to_string())))
            }
            PropName::Str(value) => Ok(self.constant(Constant::String(
                format!("{:?}", value.value).trim_matches('"').to_string(),
            ))),
            PropName::Num(value) => Ok(self.constant(Constant::Number(value.value))),
            PropName::Computed(value) => self.lower_expression(&value.expr),
            PropName::BigInt(value) => Ok(self.constant(Constant::String(value.value.to_string()))),
        }
    }

    fn lower_prefixed_property_name(&mut self, prefix: &str, name: &PropName) -> Result<Register> {
        let key = self.lower_property_name(name)?;
        if prefix.is_empty() {
            return Ok(key);
        }
        let prefix = self.constant(Constant::String(prefix.into()));
        let dst = self.register();
        self.emit(Instruction::Add {
            dst,
            lhs: prefix,
            rhs: key,
        });
        Ok(dst)
    }

    fn lower_member_parts(&mut self, member: &MemberExpr) -> Result<(Register, Register)> {
        let object = self.lower_expression(&member.obj)?;
        let key = self.lower_member_key(&member.prop)?;
        Ok((object, key))
    }

    fn lower_member_key(&mut self, property: &MemberProp) -> Result<Register> {
        match property {
            MemberProp::Ident(identifier) => {
                Ok(self.constant(Constant::String(identifier.sym.to_string())))
            }
            MemberProp::Computed(computed) => self.lower_expression(&computed.expr),
            MemberProp::PrivateName(_) => Err(anyhow!(
                "private fields require private-member W3IR lowering"
            )),
        }
    }

    fn lower_private_member_parts(
        &mut self,
        member: &MemberExpr,
    ) -> Result<(Register, Register, Register)> {
        let MemberProp::PrivateName(private_name) = &member.prop else {
            return Err(anyhow!(
                "runtime private member lowering requires a private name"
            ));
        };
        self.find_binding("*class-brand*")
            .ok_or_else(|| anyhow!("private member used outside its declaring class"))?;
        let brand_binding = self.resolve_binding("*class-brand*");
        let object = self.lower_expression(&member.obj)?;
        let brand = self.load_binding(brand_binding);
        let name = self.constant(Constant::String(private_name.name.to_string()));
        Ok((object, brand, name))
    }

    fn lower_super_property_key(&mut self, property: &SuperPropExpr) -> Result<Register> {
        match &property.prop {
            SuperProp::Ident(identifier) => {
                Ok(self.constant(Constant::String(identifier.sym.to_string())))
            }
            SuperProp::Computed(computed) => self.lower_expression(&computed.expr),
        }
    }

    fn lower_super_property(&mut self, property: &SuperPropExpr) -> Result<Register> {
        let target = self.lower_super_assignment_target(property)?;
        Ok(self.load_assignment_target(target))
    }

    fn lower_super_assignment_target(
        &mut self,
        property: &SuperPropExpr,
    ) -> Result<LoweredAssignmentTarget> {
        if self.find_binding("*super*").is_none() || self.find_binding("*this*").is_none() {
            return Err(anyhow!(
                "super property access used outside a derived runtime class method"
            ));
        }
        let parent_binding = self.resolve_binding("*super*");
        let this_binding = self.resolve_binding("*this*");
        let parent = self.load_binding(parent_binding);
        let receiver = self.load_binding(this_binding);
        let key = self.lower_super_property_key(property)?;
        Ok(LoweredAssignmentTarget::Super {
            parent,
            receiver,
            key,
            is_static: self.class_super_is_static == Some(true),
        })
    }

    fn lower_literal(&mut self, literal: &Lit) -> Result<Register> {
        let constant = match literal {
            Lit::Str(value) => Constant::String(atom_to_string(&value.value)),
            Lit::Num(value) => Constant::Number(value.value),
            Lit::Bool(value) => Constant::Bool(value.value),
            Lit::Null(_) => Constant::Null,
            Lit::Regex(value) => {
                let constructor = self.resolve_binding("RegExp");
                let constructor = self.load_binding(constructor);
                let source = self.constant(Constant::String(
                    format!("{:?}", value.exp).trim_matches('"').to_string(),
                ));
                let flags = self.constant(Constant::String(
                    format!("{:?}", value.flags).trim_matches('"').to_string(),
                ));
                let dst = self.register();
                self.emit(Instruction::Construct {
                    dst,
                    constructor,
                    arguments: vec![source, flags],
                });
                return Ok(dst);
            }
            Lit::BigInt(value) => {
                let constructor = self.resolve_binding("BigInt");
                let constructor = self.load_binding(constructor);
                let text = self.constant(Constant::String(value.value.to_string()));
                let this_value = self.constant(Constant::Undefined);
                let dst = self.register();
                self.emit(Instruction::Call {
                    dst,
                    callee: constructor,
                    this_value,
                    arguments: vec![text],
                });
                return Ok(dst);
            }
            _ => return Err(anyhow!("unsupported literal in runtime W3IR")),
        };
        Ok(self.constant(constant))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use w3cos_core::Value;
    use w3cos_vm::{Limits, Vm, VmError};

    #[test]
    fn module_init_prunes_unused_external_capture_adapters() {
        let parsed = parse(
            "sink(() => used);",
            "https://example.test/module-init-pruning.js",
        )
        .unwrap();
        let statements = parsed
            .body
            .into_iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(statement) => Some(statement),
                ModuleItem::ModuleDecl(_) => None,
            })
            .collect::<Vec<_>>();
        let mut externals = vec![("sink".to_string(), false), ("used".to_string(), false)];
        externals.extend((0..64).map(|index| (format!("unused{index}"), true)));

        let module = lower_module_statements(
            &statements,
            "https://example.test/module-init-pruning.js",
            &externals,
        )
        .unwrap();
        let imported = module
            .imports
            .iter()
            .map(|import| import.imported.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(imported, HashSet::from(["sink", "used"]));
        let entry = module
            .functions
            .iter()
            .find(|function| function.id == module.entry)
            .unwrap();
        assert!(
            entry
                .bindings
                .iter()
                .all(|binding| !binding.name.starts_with("unused")),
            "unused external bindings must not inflate AOT frames"
        );
    }

    #[test]
    fn object_methods_accessors_and_generators_share_w3ir_closures() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                const computed = "double";
                const object = {
                    base: 2,
                    add(value) {
                        return this.base + value;
                    },
                    get value() {
                        return this.base * 2;
                    },
                    set value(next) {
                        this.base = next;
                    },
                    [computed](value) {
                        return this.base * value;
                    },
                    *values() {
                        yield this.base;
                    },
                    1n: "bigint-key"
                };
                const before = object.value;
                object.value = 5;
                callback(
                    object.add(3),
                    before,
                    object.value,
                    object.double(3),
                    object.values().next().value,
                    object["1"]
                );
            "#,
            "https://example.test/object-methods.js",
        )
        .unwrap();
        assert!(
            module
                .functions
                .iter()
                .filter(|function| {
                    function
                        .name
                        .as_deref()
                        .is_some_and(|name| matches!(name, "add" | "get value" | "set value"))
                })
                .count()
                >= 3
        );

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(8.0),
                Value::Number(4.0),
                Value::Number(10.0),
                Value::Number(15.0),
                Value::Number(5.0),
                Value::string("bigint-key"),
            ]
        );
    }

    #[test]
    fn call_method_construct_optional_and_super_spreads_share_materialized_arguments() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                function collect(...values) {
                    return values.join(":");
                }
                const receiver = {
                    prefix: "method",
                    join: function(...values) {
                        return this.prefix + ":" + values.join(":");
                    }
                };
                class Box {
                    constructor(...values) {
                        this.value = values.join(":");
                    }
                }
                class Parent {
                    constructor(...values) {
                        this.value = values.join(":");
                    }
                    join(...values) {
                        return this.value + "|" + values.join(":");
                    }
                }
                class Child extends Parent {
                    constructor(values) {
                        super("base", ...values);
                    }
                    call(values) {
                        return super.join("next", ...values);
                    }
                }
                const values = [1, 2];
                let skipped = 0;
                const missing = null;
                const optional = missing?.(...[skipped += 1]);
                const child = new Child(["x", "y"]);
                callback(
                    collect(0, ...values, 3),
                    receiver.join(...values),
                    new Box("a", ...["b", "c"]).value,
                    optional,
                    skipped,
                    child.call(["z"])
                );
            "#,
            "https://example.test/call-spread.js",
        )
        .unwrap();
        let instructions = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .collect::<Vec<_>>();
        assert!(
            instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::CallWithArguments { .. }))
        );
        assert!(
            instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::CallMethodWithArguments { .. }
            ))
        );
        assert!(
            instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::ConstructWithArguments { .. }
            ))
        );

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string("0:1:2:3"),
                Value::string("method:1:2"),
                Value::string("a:b:c"),
                Value::Undefined,
                Value::Number(0.0),
                Value::string("base:x:y|next:z"),
            ]
        );
    }

    #[test]
    fn array_spread_appends_iterables_in_source_order_in_w3vm() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                let trace = "";
                function mark(value) {
                    trace += value;
                    return value;
                }
                const values = [
                    mark("a"),
                    ...[mark("b"), mark("c")],
                    mark("d"),
                    ..."xy"
                ];
                let rejected;
                try {
                    const invalid = [...{}];
                    rejected = "missing";
                } catch (error) {
                    rejected = error.name;
                }
                callback(trace, values.join(":"), rejected);
            "#,
            "https://example.test/array-spread.js",
        )
        .unwrap();
        assert!(
            module
                .functions
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(instruction, Instruction::AppendIterable { .. }))
        );

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string("abcd"),
                Value::string("a:b:c:d:x:y"),
                Value::string("TypeError"),
            ]
        );
    }

    #[test]
    fn sparse_arrays_preserve_empty_slots_across_spread_and_callbacks() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                const values = [1, , 3, ...[, 5]];
                const visited = [];
                values.forEach((value, index) => visited.push(index));
                const enumerable = [];
                for (const key in values) enumerable.push(key);
                const mapped = values.map(value => value * 2);
                callback(
                    values.length,
                    1 in values,
                    values[1],
                    3 in values,
                    values[3],
                    visited.join(","),
                    enumerable.join(","),
                    1 in mapped,
                    mapped.length
                );
            "#,
            "https://example.test/sparse-arrays.js",
        )
        .unwrap();
        assert!(
            module
                .functions
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(instruction, Instruction::DeleteProperty { .. }))
        );

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(5.0),
                Value::Bool(false),
                Value::Undefined,
                Value::Bool(true),
                Value::Undefined,
                Value::string("0,2,3,4"),
                Value::string("0,2,3,4"),
                Value::Bool(false),
                Value::Number(5.0),
            ]
        );
    }

    #[test]
    fn object_and_jsx_spreads_share_copy_data_properties_in_w3vm() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                const source = { kept: "source", added: 2 };
                const merged = { before: 1, ...null, ...source, kept: "own" };
                const attributes = { role: "button", tabIndex: 1, children: "spread" };
                const node = <section {...attributes} tabIndex={2}>child</section>;
                callback(
                    merged.before,
                    merged.kept,
                    merged.added,
                    node.type,
                    node.props.role,
                    node.props.tabIndex,
                    node.props.children[0]
                );
            "#,
            "https://example.test/spreads.jsx",
        )
        .unwrap();
        let copy_count = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction, Instruction::CopyDataProperties { .. }))
            .count();
        assert_eq!(copy_count, 3);

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(1.0),
                Value::string("own"),
                Value::Number(2.0),
                Value::string("section"),
                Value::string("button"),
                Value::Number(2.0),
                Value::string("child"),
            ]
        );
    }

    #[test]
    fn delete_and_in_share_w3ir_property_semantics() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                const object = { x: 1 };
                let sideEffects = 0;
                const deleted = delete object.x;
                const nonReference = delete (sideEffects += 1);
                const absent = delete object.missing;
                callback(deleted, "x" in object, nonReference, sideEffects, absent);
            "#,
            "https://example.test/delete.js",
        )
        .unwrap();
        assert!(
            module
                .functions
                .iter()
                .flat_map(|function| &function.blocks)
                .any(|block| block
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction, Instruction::DeleteProperty { .. })))
        );
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(true),
                Value::Number(1.0),
                Value::Bool(true),
            ]
        );
    }

    #[test]
    fn optional_chains_preserve_receivers_and_skip_arguments_in_w3vm() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                let calls = 0;
                const missing = null;
                const skipped = missing?.method(calls += 1);
                const object = {
                    value: 7,
                    method: function() { return this.value; }
                };
                callback(skipped, calls, object?.method());
            "#,
            "https://example.test/optional.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[Value::Undefined, Value::Number(0.0), Value::Number(7.0)]
        );
    }

    #[test]
    fn external_script_mutates_the_host_dom_object_without_rustc() {
        let module = lower_script(
            r#"document.body.setAttribute("data-ready", "yes");"#,
            "https://example.test/app.js",
        )
        .unwrap();
        let document_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "document")
            .unwrap()
            .local;

        let attributes = Rc::new(RefCell::new(HashMap::<String, String>::new()));
        let attributes_for_call = attributes.clone();
        let body = Value::object(HashMap::new());
        body.set_property(
            "setAttribute",
            Value::function(move |this_value, arguments| {
                assert!(this_value.is_object());
                attributes_for_call
                    .borrow_mut()
                    .insert(arguments[0].to_js_string(), arguments[1].to_js_string());
                Value::Undefined
            }),
        );
        let document = Value::object(HashMap::from([("body".into(), body)]));

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(document_binding, document)]))
            .unwrap();
        assert_eq!(
            attributes.borrow().get("data-ready").map(String::as_str),
            Some("yes")
        );
    }

    #[test]
    fn async_generators_share_await_and_yield_suspension_metadata() {
        let module = lower_script(
            "async function* values() { yield await Promise.resolve(1); }",
            "inline:test",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("values"))
            .unwrap();
        assert!(function.is_async);
        assert!(function.is_generator);
        assert_eq!(function.suspension_points.len(), 1);
        assert_eq!(function.generator_suspension_points.len(), 1);
    }

    #[test]
    fn anonymous_function_expression_uses_its_variable_binding_as_w3ir_identity() {
        let module = lower_module(
            "export const values = function* () { yield 1; };",
            "inline:generator-expression",
        )
        .unwrap();
        let function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("values"))
            .expect("anonymous function expression should inherit its binding name");
        assert!(function.is_generator);
        assert_eq!(function.generator_suspension_points.len(), 1);
    }

    #[test]
    fn arrow_functions_use_their_variable_bindings_as_w3ir_identities() {
        let module = lower_module(
            "export const identity = (value) => value;
             export const load = async () => await Promise.resolve('ready');",
            "inline:arrow-bindings",
        )
        .unwrap();

        let identity = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("identity"))
            .expect("synchronous arrow should inherit its binding name");
        assert!(!identity.is_async);
        assert!(!identity.is_generator);

        let load = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("load"))
            .expect("async arrow should inherit its binding name");
        assert!(load.is_async);
        assert!(!load.is_generator);
        assert_eq!(load.suspension_points.len(), 1);
    }

    #[test]
    fn instance_and_static_generator_methods_have_distinct_w3ir_identities() {
        let module = lower_module(
            "const first = 'a'; const second = 'b'; export class Counter {
                *values() { yield 1; }
                static *values() { yield 2; }
                *[first]() { yield 3; }
                static *[second]() { yield 4; }
            }",
            "inline:class-generators",
        )
        .unwrap();
        for name in [
            "Counter.values",
            "Counter.static values",
            "Counter.<computed_1>",
            "Counter.static <computed_2>",
        ] {
            let function = module
                .functions
                .iter()
                .find(|function| function.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("missing W3IR function {name}"));
            assert!(function.is_generator);
            assert_eq!(function.generator_suspension_points.len(), 1);
        }
    }

    #[test]
    fn getters_and_setters_have_distinct_w3ir_identities() {
        let module = lower_module(
            "export class Box {
                get value() { return this.current; }
                set value(next) { this.current = next; }
                static get value() { return 1; }
                static set value(next) { this.current = next; }
                get #secret() { return this.current; }
                set #secret(next) { this.current = next; }
            }",
            "inline:class-accessors",
        )
        .unwrap();

        for name in [
            "Box.get value",
            "Box.set value",
            "Box.static get value",
            "Box.static set value",
            "Box.get #secret",
            "Box.set #secret",
        ] {
            assert!(
                module
                    .functions
                    .iter()
                    .any(|function| function.name.as_deref() == Some(name)),
                "missing distinct W3IR accessor {name}"
            );
        }
    }

    #[test]
    fn async_generator_requests_queue_across_await_and_yield() {
        let module = lower_script(
            r#"
                let trace = "";
                async function* values() {
                    trace += "S";
                    const sent = yield await Promise.resolve("one");
                    trace += sent;
                    yield Promise.resolve("two");
                    return Promise.resolve("end");
                }
                const generator = values();
                callback("lazy", trace, generator[Symbol.asyncIterator]() === generator);
                generator.next().then(result =>
                    callback("first", result.value, result.done, trace));
                generator.next("A").then(result =>
                    callback("second", result.value, result.done, trace));
                generator.next().then(result =>
                    callback("third", result.value, result.done, trace));
            "#,
            "https://example.test/async-generator.js",
        )
        .unwrap();
        let promise_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "Promise")
            .expect("Promise import")
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let symbol_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "Symbol")
            .expect("Symbol import")
            .local;

        let promise = Value::object(HashMap::new());
        promise.set_property(
            "resolve",
            Value::function(|_, arguments| w3cos_core::promise::resolve(arguments)),
        );
        let symbol = Value::object(HashMap::from([(
            "asyncIterator".into(),
            Value::string("__w3cos_symbol_async_iterator"),
        )]));
        let observed = Rc::new(RefCell::new(Vec::<Vec<Value>>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments);
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (promise_binding, promise),
                (callback_binding, callback),
                (symbol_binding, symbol),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                vec![Value::string("lazy"), Value::string(""), Value::Bool(true),],
                vec![
                    Value::string("first"),
                    Value::string("one"),
                    Value::Bool(false),
                    Value::string("SA"),
                ],
                vec![
                    Value::string("second"),
                    Value::string("two"),
                    Value::Bool(false),
                    Value::string("SA"),
                ],
                vec![
                    Value::string("third"),
                    Value::string("end"),
                    Value::Bool(true),
                    Value::string("SA"),
                ],
            ]
        );
    }

    #[test]
    fn async_yield_delegate_forwards_queued_throw_completion() {
        let module = lower_script(
            r#"
                let trace = "";
                async function* inner() {
                    try {
                        const sent = yield Promise.resolve("inner");
                        return "end:" + sent;
                    } catch (error) {
                        yield "caught:" + error;
                    } finally {
                        trace += "F";
                    }
                    return "done";
                }
                async function* outer() {
                    return yield* inner();
                }
                const generator = outer();
                generator.next().then(result =>
                    callback("first", result.value, result.done));
                generator.throw("boom").then(result =>
                    callback("caught", result.value, result.done));
                generator.next().then(result =>
                    callback("done", result.value, result.done, trace));
            "#,
            "https://example.test/async-yield-delegate.js",
        )
        .unwrap();
        let promise_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "Promise")
            .expect("Promise import")
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let promise = Value::object(HashMap::new());
        promise.set_property(
            "resolve",
            Value::function(|_, arguments| w3cos_core::promise::resolve(arguments)),
        );
        let observed = Rc::new(RefCell::new(Vec::<Vec<Value>>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments);
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (promise_binding, promise),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                vec![
                    Value::string("first"),
                    Value::string("inner"),
                    Value::Bool(false),
                ],
                vec![
                    Value::string("caught"),
                    Value::string("caught:boom"),
                    Value::Bool(false),
                ],
                vec![
                    Value::string("done"),
                    Value::string("done"),
                    Value::Bool(true),
                    Value::string("F"),
                ],
            ]
        );
    }

    #[test]
    fn for_await_inside_async_generator_awaits_values_and_iterator_close() {
        let module = lower_script(
            r#"
                let trace = "";
                async function* source() {
                    try {
                        yield Promise.resolve(1);
                        yield 2;
                    } finally {
                        trace += "C";
                    }
                }
                async function* collect() {
                    for await (const value of source()) {
                        yield value + 10;
                        break;
                    }
                    return trace;
                }
                const generator = collect();
                generator.next().then(result =>
                    callback("value", result.value, result.done));
                generator.next().then(result =>
                    callback("closed", result.value, result.done, trace));
            "#,
            "https://example.test/async-generator-for-await.js",
        )
        .unwrap();
        let promise_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "Promise")
            .expect("Promise import")
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let promise = Value::object(HashMap::new());
        promise.set_property(
            "resolve",
            Value::function(|_, arguments| w3cos_core::promise::resolve(arguments)),
        );
        let observed = Rc::new(RefCell::new(Vec::<Vec<Value>>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments);
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (promise_binding, promise),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                vec![
                    Value::string("value"),
                    Value::Number(11.0),
                    Value::Bool(false),
                ],
                vec![
                    Value::string("closed"),
                    Value::string("C"),
                    Value::Bool(true),
                    Value::string("C"),
                ],
            ]
        );
    }

    #[test]
    fn generator_frames_are_lazy_resumable_iterators_with_completion_injection() {
        let module = lower_script(
            r#"
                let trace = "";
                function* values() {
                    try {
                        trace += "S";
                        const sent = yield 1;
                        trace += sent;
                        yield 2;
                        return 3;
                    } finally {
                        trace += "F";
                    }
                }
                const generator = values();
                const before = trace;
                const first = generator.next("ignored");
                const second = generator.next("A");
                const returned = generator.return(9);
                const completed = generator.next();

                function* caught() {
                    try {
                        yield "start";
                    } catch (error) {
                        yield "caught:" + error;
                    } finally {
                        trace += "C";
                    }
                    return "end";
                }
                const throwing = caught();
                const throwStart = throwing.next();
                const throwCaught = throwing.throw("boom");
                const throwDone = throwing.next();

                function* cleanupYield() {
                    try {
                        yield "body";
                    } finally {
                        yield "cleanup";
                    }
                }
                const cleanup = cleanupYield();
                cleanup.next();
                const cleanupStep = cleanup.return("returned");
                const cleanupDone = cleanup.next();

                callback(
                    before,
                    first.value, first.done,
                    second.value, second.done,
                    returned.value, returned.done,
                    completed.value, completed.done,
                    throwStart.value,
                    throwCaught.value, throwCaught.done,
                    throwDone.value, throwDone.done,
                    cleanupStep.value, cleanupStep.done,
                    cleanupDone.value, cleanupDone.done,
                    trace,
                    generator[Symbol.iterator]() === generator
                );
            "#,
            "https://example.test/generator.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let symbol_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "Symbol")
            .expect("Symbol import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });
        let symbol = Value::object(HashMap::from([(
            "iterator".into(),
            Value::string("__w3cos_symbol_iterator"),
        )]));

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (callback_binding, callback),
                (symbol_binding, symbol),
            ]))
            .unwrap();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string(""),
                Value::Number(1.0),
                Value::Bool(false),
                Value::Number(2.0),
                Value::Bool(false),
                Value::Number(9.0),
                Value::Bool(true),
                Value::Undefined,
                Value::Bool(true),
                Value::string("start"),
                Value::string("caught:boom"),
                Value::Bool(false),
                Value::string("end"),
                Value::Bool(true),
                Value::string("cleanup"),
                Value::Bool(false),
                Value::string("returned"),
                Value::Bool(true),
                Value::string("SAFC"),
                Value::Bool(true),
            ]
        );
    }

    #[test]
    fn yield_delegate_forwards_next_throw_and_return_completions() {
        let module = lower_script(
            r#"
                function* inner() {
                    const received = yield "inner";
                    return "end:" + received;
                }
                function* outer() {
                    return yield* inner();
                }
                const nextDelegate = outer();
                const nextFirst = nextDelegate.next();
                const nextDone = nextDelegate.next("sent");

                const returnDelegate = outer();
                returnDelegate.next();
                const returnDone = returnDelegate.return("stopped");

                function* catchingInner() {
                    try {
                        yield "start";
                    } catch (error) {
                        yield "caught:" + error;
                        return "recovered";
                    }
                }
                function* catchingOuter() {
                    return yield* catchingInner();
                }
                const throwDelegate = catchingOuter();
                throwDelegate.next();
                const throwCaught = throwDelegate.throw("boom");
                const throwDone = throwDelegate.next();

                callback(
                    nextFirst.value, nextFirst.done,
                    nextDone.value, nextDone.done,
                    returnDone.value, returnDone.done,
                    throwCaught.value, throwCaught.done,
                    throwDone.value, throwDone.done
                );
            "#,
            "https://example.test/yield-delegate.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string("inner"),
                Value::Bool(false),
                Value::string("end:sent"),
                Value::Bool(true),
                Value::string("stopped"),
                Value::Bool(true),
                Value::string("caught:boom"),
                Value::Bool(false),
                Value::string("recovered"),
                Value::Bool(true),
            ]
        );
    }

    #[test]
    fn catch_binding_patterns_use_shared_w3ir_destructuring() {
        let module = lower_script(
            r#"
                const key = "message";
                try {
                    throw {
                        message: "boom",
                        values: [void 0, 4, 5, 6],
                        extra: 9
                    };
                } catch ({
                    [key]: text,
                    values: [first = "fallback", second, ...tail],
                    ...rest
                }) {
                    callback(text, first, second, tail.join(":"), rest.extra);
                }
                try {
                    throw [void 0, { code: 7 }, 8, 9];
                } catch ([first = "default", { code }, ...tail]) {
                    callback(first, code, tail.join(":"));
                }
                try {
                    try {
                        throw {};
                    } catch ({ [later]: value, later = "key" }) {
                        callback("unreachable");
                    }
                } catch (error) {
                    callback(error.name);
                }
            "#,
            "https://example.test/catch-patterns.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Vec<Value>>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments);
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                vec![
                    Value::string("boom"),
                    Value::string("fallback"),
                    Value::Number(4.0),
                    Value::string("5:6"),
                    Value::Number(9.0),
                ],
                vec![
                    Value::string("default"),
                    Value::Number(7.0),
                    Value::string("8:9"),
                ],
                vec![Value::string("ReferenceError")],
            ]
        );
    }

    #[test]
    fn empty_try_body_does_not_emit_an_empty_exception_region() {
        let module = lower_script(
            r#"
                try {
                } catch (error) {
                    callback(error);
                } finally {
                    callback("finally");
                }
            "#,
            "https://example.test/empty-try.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments[0].clone());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &[Value::string("finally")]);
    }

    #[test]
    fn try_catch_finally_preserves_and_overrides_abrupt_completions() {
        let module = lower_script(
            r#"
                let trace = "";
                function returned() {
                    try {
                        trace += "T";
                        return 1;
                    } finally {
                        trace += "F";
                    }
                }
                function overridden() {
                    try {
                        return 1;
                    } finally {
                        return 2;
                    }
                }
                function caught() {
                    try {
                        throw "boom";
                    } catch (error) {
                        trace += ":" + error;
                        return "caught";
                    } finally {
                        trace += ":done";
                    }
                }
                function hostCaught() {
                    try {
                        hostThrow();
                    } catch (error) {
                        trace += ":host-" + error;
                        return "host-caught";
                    } finally {
                        trace += ":host-done";
                    }
                }
                function finallyThrowOverridesReturn() {
                    try {
                        try {
                            return "lost";
                        } finally {
                            throw "override";
                        }
                    } catch (error) {
                        return error;
                    }
                }
                function throwSurvivesFinally() {
                    try {
                        try {
                            throw "preserved";
                        } finally {
                            trace += ":throw-finally";
                        }
                    } catch (error) {
                        return error;
                    }
                }
                let loopTrace = "";
                for (let index = 0; index < 3; index++) {
                    try {
                        if (index === 0) continue;
                        if (index === 1) break;
                    } finally {
                        loopTrace += index;
                    }
                }
                callback(
                    returned(),
                    overridden(),
                    caught(),
                    hostCaught(),
                    finallyThrowOverridesReturn(),
                    throwSurvivesFinally(),
                    trace,
                    loopTrace
                );
            "#,
            "https://example.test/try-finally.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let host_throw_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "hostThrow")
            .expect("hostThrow import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });
        let host_throw = Value::function(|_, _| {
            w3cos_core::throw_value(Value::string("native"));
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (callback_binding, callback),
                (host_throw_binding, host_throw),
            ]))
            .unwrap();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(1.0),
                Value::Number(2.0),
                Value::string("caught"),
                Value::string("host-caught"),
                Value::string("override"),
                Value::string("preserved"),
                Value::string("TF:boom:done:host-native:host-done:throw-finally"),
                Value::string("01"),
            ]
        );
    }

    #[test]
    fn await_inside_try_catch_finally_resumes_with_completion_intact() {
        let module = lower_script(
            r#"
                async function resolved() {
                    let trace = "";
                    try {
                        trace += await Promise.resolve("A");
                        return trace + "R";
                    } finally {
                        trace += await Promise.resolve("F");
                        callback("resolved", trace);
                    }
                }
                async function rejected() {
                    let trace = "";
                    try {
                        await Promise.reject("E");
                    } catch (error) {
                        trace += "C" + error;
                        return trace;
                    } finally {
                        trace += await Promise.resolve("F");
                        callback("rejected", trace);
                    }
                }
                resolved();
                rejected();
            "#,
            "https://example.test/async-try-finally.js",
        )
        .unwrap();
        let promise_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "Promise")
            .expect("Promise import")
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;

        let promise = Value::object(HashMap::new());
        promise.set_property(
            "resolve",
            Value::function(|_, arguments| w3cos_core::promise::resolve(arguments)),
        );
        promise.set_property(
            "reject",
            Value::function(|_, arguments| w3cos_core::promise::reject(arguments)),
        );
        let observed = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed
                .borrow_mut()
                .push((arguments[0].to_js_string(), arguments[1].to_js_string()));
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (promise_binding, promise),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                ("resolved".into(), "AF".into()),
                ("rejected".into(), "CEF".into()),
            ]
        );
    }

    #[test]
    fn logical_conditional_and_basic_unary_expressions_share_w3ir_control_flow() {
        let module = lower_script(
            r#"
                callback(
                    0 || 7,
                    "left" && "right",
                    null ?? "fallback",
                    true ? "yes" : "no",
                    !0,
                    +"12",
                    -3,
                    void callback("side-effect")
                );
            "#,
            "https://example.test/map-style-expressions.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let calls = Rc::new(RefCell::new(Vec::<Vec<Value>>::new()));
        let observed_calls = Rc::clone(&calls);
        let callback = Value::function(move |_, arguments| {
            observed_calls.borrow_mut().push(arguments);
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();

        let calls = calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], vec![Value::string("side-effect")]);
        assert_eq!(
            calls[1],
            vec![
                Value::Number(7.0),
                Value::string("right"),
                Value::string("fallback"),
                Value::string("yes"),
                Value::Bool(true),
                Value::Number(12.0),
                Value::Number(-3.0),
                Value::Undefined,
            ]
        );
    }

    #[test]
    fn templates_and_typescript_expression_wrappers_share_w3ir_addition() {
        let module = lower_script(
            r#"
                let order = "";
                function read(label, value) {
                    order += label;
                    return value;
                }
                function identity(value) {
                    return value;
                }

                const count = 3 as number;
                const text =
                    (`head\n${read("A", count)}:${read("B", null)}:\`\u{1F600}`)!;
                const satisfied = count satisfies number;
                const asserted = <string>`assert:${count}`;
                const cast = `cast:${count}` as const;
                const instantiated = (identity<string>)("typed");
                const quoted = "class=\"";

                callback(
                    text,
                    order,
                    satisfied,
                    asserted,
                    cast,
                    instantiated,
                    quoted
                );
            "#,
            "https://example.test/templates.ts",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string("head\n3:null:`😀"),
                Value::string("AB"),
                Value::Number(3.0),
                Value::string("assert:3"),
                Value::string("cast:3"),
                Value::string("typed"),
                Value::string("class=\""),
            ]
        );
    }

    #[test]
    fn typescript_type_only_declarations_are_erased_without_runtime_bindings() {
        let module = lower_script(
            r#"
                interface Shape { size: number }
                type ShapeName = string;
                declare function ambientCall(value: number): number;
                declare class AmbientClass { value: number }
                declare const ambientValue: number;
                declare enum AmbientEnum { Ready }
                declare namespace AmbientNamespace {
                    const ready: boolean;
                }
                const value: Shape["size"] = 3;
                callback(value);
            "#,
            "https://example.test/type-only.ts",
        )
        .unwrap();
        for erased in [
            "Shape",
            "ShapeName",
            "ambientCall",
            "AmbientClass",
            "ambientValue",
            "AmbientEnum",
            "AmbientNamespace",
        ] {
            assert!(
                module.functions[0]
                    .bindings
                    .iter()
                    .all(|binding| binding.name != erased),
                "{erased} must not create a runtime binding"
            );
        }
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    *callback_observed.borrow_mut() =
                        arguments.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(*observed.borrow(), Value::Number(3.0));

        let esm = lower_module(
            r#"
                import type { RemoteShape } from "./types.js";
                export interface Shape { size: number }
                export type ShapeName = string;
                export type { RemoteShape } from "./types.js";
                export default interface DefaultShape { ready: boolean }
                export declare function ambientCall(value: number): number;
                export declare class AmbientClass { value: number }
                export declare const ambientValue: number;
                export declare enum AmbientEnum { Ready }
                export declare namespace AmbientNamespace {
                    const ready: boolean;
                }
                declare global {
                    interface Window { runtimeReady: boolean }
                }
                export const value = 4;
            "#,
            "https://example.test/type-only.mts",
        )
        .unwrap();
        assert!(esm.requested_modules.is_empty());
        assert_eq!(
            esm.exports
                .iter()
                .map(|export| export.exported.as_str())
                .collect::<Vec<_>>(),
            vec!["value"]
        );

        for source in [
            "enum RuntimeEnum { Ready }",
            "namespace RuntimeNamespace { export const ready = true; }",
        ] {
            assert!(
                lower_script(source, "https://example.test/runtime-declaration.ts").is_err(),
                "TypeScript declarations with runtime semantics must remain explicit errors"
            );
        }
    }

    #[test]
    fn logical_and_exponent_assignments_evaluate_w3ir_targets_once() {
        let module = lower_script(
            r#"
                let keyCalls = 0;
                let rhsCalls = 0;
                function key() {
                    keyCalls += 1;
                    return "value";
                }
                function rhs(value) {
                    rhsCalls += 1;
                    return value;
                }

                const box = { value: 0, truthy: 1, nullish: null };
                const first = (box[key()] ||= rhs(2));
                const second = (box[key()] ||= rhs(3));
                const third = (box.truthy &&= rhs(4));
                const fourth = (box.nullish ??= rhs(5));

                let identifier = 0;
                identifier &&= rhs(6);
                identifier ||= rhs(7);
                identifier ??= rhs(8);

                let power = 2;
                power **= 3;

                const immutableSkipped = 0;
                immutableSkipped &&= rhs(11);
                let immutableResult = "missed";
                try {
                    const immutableTaken = 1;
                    immutableTaken &&= 2;
                } catch (error) {
                    immutableResult = "caught";
                }

                class PrivateBox {
                    #value = 0;
                    update() {
                        this.#value ||= rhs(9);
                        this.#value &&= this.#value + 1;
                        this.#value ??= rhs(10);
                        return this.#value;
                    }
                }

                callback(
                    first,
                    second,
                    third,
                    fourth,
                    identifier,
                    power,
                    new PrivateBox().update(),
                    keyCalls,
                    rhsCalls,
                    immutableSkipped,
                    immutableResult
                );
            "#,
            "https://example.test/logical-assignments.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(2.0),
                Value::Number(2.0),
                Value::Number(4.0),
                Value::Number(5.0),
                Value::Number(7.0),
                Value::Number(8.0),
                Value::Number(10.0),
                Value::Number(2.0),
                Value::Number(5.0),
                Value::Number(0.0),
                Value::string("caught"),
            ]
        );
    }

    #[test]
    fn member_calls_route_builtin_arrays_through_shared_core_semantics() {
        let module = lower_script(
            r#"
                const values = [];
                const firstLength = values.push(2);
                const finalLength = values.push(3, 4);
                const doubled = values.map(value => value * 2);
                callback(
                    firstLength,
                    finalLength,
                    values.join(":"),
                    doubled.join(":")
                );
            "#,
            "https://example.test/array-methods.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(1.0),
                Value::Number(3.0),
                Value::string("2:3:4"),
                Value::string("4:6:8"),
            ]
        );
    }

    #[test]
    fn map_style_loops_updates_compound_assignment_and_typeof_share_w3ir() {
        let module = lower_script(
            r#"
                var total = 0;
                for (var index = 0; index < 5; index++) {
                    if (index === 2) continue;
                    total += index;
                }

                var turns = 0;
                do {
                    turns++;
                } while (turns < 2);

                const box = { value: "1" };
                const previous = box.value++;
                box.value *= 3;

                var mask = (7 & 3) | 8;
                mask ^= 2;
                mask <<= 1;
                var route = "";
                switch (mask) {
                    default:
                        route = "default";
                        break;
                    case 18:
                        route = "match";
                    case 19:
                        route += ":fallthrough";
                        break;
                }

                var nested = 0;
                for (var item = 0; item < 3; item++) {
                    switch (item) {
                        case 1:
                            continue;
                        default:
                            nested += item;
                    }
                }
                callback(
                    typeof callback,
                    total,
                    turns,
                    previous,
                    box.value,
                    (total += 1, total),
                    mask,
                    route,
                    nested,
                    -8 >> 2,
                    -1 >>> 1,
                    ~0
                );
            "#,
            "https://example.test/minified-map-loader.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .expect("callback import")
            .local;
        let observed = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();

        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string("function"),
                Value::Number(8.0),
                Value::Number(2.0),
                Value::Number(1.0),
                Value::Number(6.0),
                Value::Number(9.0),
                Value::Number(18.0),
                Value::string("match:fallthrough"),
                Value::Number(2.0),
                Value::Number(-2.0),
                Value::Number(2_147_483_647.0),
                Value::Number(-1.0),
            ]
        );
    }

    #[test]
    fn esm_imports_exports_and_reexports_lower_to_live_binding_records() {
        let module = lower_module(
            r#"
                import primary, { value as localValue } from "./dependency.js";
                export const own = localValue + primary;
                export { value as forwarded } from "./dependency.js";
                export default own;
            "#,
            "https://example.test/modules/main.js",
        )
        .unwrap();

        assert_eq!(
            module.requested_modules,
            vec!["./dependency.js".to_string()]
        );
        assert!(module.imports.iter().any(|import| {
            import.specifier == "./dependency.js" && import.imported == "default"
        }));
        assert!(
            module.imports.iter().any(|import| {
                import.specifier == "./dependency.js" && import.imported == "value"
            })
        );
        let exported: Vec<_> = module
            .exports
            .iter()
            .map(|export| export.exported.as_str())
            .collect();
        assert_eq!(exported, ["own", "forwarded", "default"]);
    }

    #[test]
    fn esm_named_and_default_classes_share_the_w3ir_class_instruction() {
        let module = lower_module(
            r#"
                export class Named {
                    value() { return 1; }
                }
                export default class Defaulted {
                    static value() { return 2; }
                }
            "#,
            "https://example.test/modules/classes.js",
        )
        .unwrap();
        let exported = module
            .exports
            .iter()
            .map(|export| export.exported.as_str())
            .collect::<Vec<_>>();
        assert_eq!(exported, ["Named", "default"]);
        assert_eq!(module.format_version, w3cos_ir::FORMAT_VERSION);
        assert_eq!(
            module.functions[0]
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, Instruction::CreateClass { .. }))
                .count(),
            2
        );
        Vm::new(module, Limits::default()).unwrap().run().unwrap();
    }

    #[test]
    fn export_star_lowers_to_a_module_linker_record() {
        let module = lower_module(
            r#"export * from "./dependency.js";"#,
            "https://example.test/modules/main.js",
        )
        .unwrap();

        assert_eq!(module.requested_modules, ["./dependency.js"]);
        assert_eq!(module.star_exports, ["./dependency.js"]);
        assert!(module.exports.is_empty());
    }

    #[test]
    fn dynamic_import_lowers_to_the_w3ir_module_instruction() {
        let module = lower_module(
            r#"import("./chunk.js").then((namespace) => callback(namespace.value));"#,
            "https://example.test/main.js",
        )
        .unwrap();
        assert!(module.functions[0].blocks.iter().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::DynamicImport { .. }))
        }));
    }

    #[test]
    fn async_arrow_await_lowers_to_a_resumable_w3ir_function() {
        let module = lower_script(
            r#"
                const run = async () => {
                    const value = await Promise.resolve("ready");
                    callback(value);
                };
                run();
            "#,
            "https://example.test/async.js",
        )
        .unwrap();
        let async_function = module
            .functions
            .iter()
            .find(|function| function.name.as_deref() == Some("run"))
            .expect("async function");
        assert!(async_function.is_async);
        assert_eq!(async_function.suspension_points.len(), 1);
        assert!(async_function.blocks.iter().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Await { .. }))
        }));
    }

    #[test]
    fn top_level_await_marks_the_module_entry_as_async() {
        let module = lower_module(
            r#"export const value = await Promise.resolve("ready");"#,
            "https://example.test/module.js",
        )
        .unwrap();
        let entry = &module.functions[0];
        assert!(entry.is_async);
        assert_eq!(entry.suspension_points.len(), 1);
        assert!(entry.blocks.iter().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Await { .. }))
        }));
    }

    #[test]
    fn if_while_break_and_continue_lower_to_real_cfg_blocks() {
        let module = lower_script(
            r#"
                let index = 0;
                let total = 0;
                while (index < 5) {
                    index = index + 1;
                    if (index === 2) {
                        continue;
                    }
                    if (index === 4) {
                        break;
                    }
                    total = total + index;
                }
                callback(total);
            "#,
            "https://example.test/control-flow.js",
        )
        .unwrap();
        assert!(module.functions[0].blocks.len() >= 10);
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments[0].clone();
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(*observed.borrow(), Value::Number(4.0));
    }

    #[test]
    fn labeled_break_continue_and_blocks_lower_to_shared_cfg_targets() {
        let module = lower_script(
            r#"
                let seen = "";
                outer: for (let i = 0; i < 3; i++) {
                    for (let j = 0; j < 3; j++) {
                        if (j === 0) seen += i;
                        if (j === 1) continue outer;
                    }
                }
                section: {
                    seen += "a";
                    break section;
                    seen += "x";
                }
                seen += "b";
                let k = 0;
                alpha: beta: while (k < 2) {
                    k++;
                    continue alpha;
                }
                seen += k;
                callback(seen);
            "#,
            "https://example.test/labeled-control.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, args| {
            *callback_observed.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(*observed.borrow(), Value::string("012ab2"));
    }

    #[test]
    fn labeled_break_closes_each_crossed_sync_iterator_once() {
        let module = lower_script(
            r#"
                outer: for (const a of outerIterable) {
                    for (const b of innerIterable) {
                        break outer;
                    }
                }
            "#,
            "https://example.test/labeled-iterator-close.js",
        )
        .unwrap();
        let outer_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "outerIterable")
            .unwrap()
            .local;
        let inner_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "innerIterable")
            .unwrap()
            .local;
        let closed = Rc::new(RefCell::new(Vec::new()));
        let make_iterable = |name: &'static str| {
            let yielded = Rc::new(Cell::new(false));
            let next_yielded = Rc::clone(&yielded);
            let iterator = Value::object(HashMap::new());
            iterator.set_property(
                "next",
                Value::function(move |_, _| {
                    let done = next_yielded.replace(true);
                    w3cos_core::js_object! {
                        "value" => Value::Number(1.0),
                        "done" => Value::Bool(done),
                    }
                }),
            );
            let closed_by_return = Rc::clone(&closed);
            iterator.set_property(
                "return",
                Value::function(move |_, _| {
                    closed_by_return.borrow_mut().push(name);
                    w3cos_core::js_object! { "done" => Value::Bool(true) }
                }),
            );
            let iterable = Value::object(HashMap::new());
            iterable.set_property(
                "__w3cos_symbol_iterator",
                Value::function(move |_, _| iterator.clone()),
            );
            iterable
        };
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (outer_binding, make_iterable("outer")),
                (inner_binding, make_iterable("inner")),
            ]))
            .unwrap();
        assert_eq!(closed.borrow().as_slice(), &["inner", "outer"]);
    }

    #[test]
    fn labeled_break_awaits_each_crossed_async_iterator_close() {
        let module = lower_module(
            r#"
                outer: for await (const a of outerIterable) {
                    for await (const b of innerIterable) {
                        break outer;
                    }
                }
                callback("done");
            "#,
            "https://example.test/labeled-async-iterator-close.js",
        )
        .unwrap();
        let outer_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "outerIterable")
            .unwrap()
            .local;
        let inner_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "innerIterable")
            .unwrap()
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let closed = Rc::new(RefCell::new(Vec::new()));
        let make_iterable = |name: &'static str| {
            let yielded = Rc::new(Cell::new(false));
            let next_yielded = Rc::clone(&yielded);
            let iterator = Value::object(HashMap::new());
            iterator.set_property(
                "next",
                Value::function(move |_, _| {
                    let done = next_yielded.replace(true);
                    w3cos_core::promise::resolve(vec![w3cos_core::js_object! {
                        "value" => Value::Number(1.0),
                        "done" => Value::Bool(done),
                    }])
                }),
            );
            let closed_by_return = Rc::clone(&closed);
            iterator.set_property(
                "return",
                Value::function(move |_, _| {
                    closed_by_return.borrow_mut().push(name);
                    w3cos_core::promise::resolve(vec![
                        w3cos_core::js_object! { "done" => Value::Bool(true) },
                    ])
                }),
            );
            let iterable = Value::object(HashMap::new());
            iterable.set_property(
                "__w3cos_symbol_async_iterator",
                Value::function(move |_, _| iterator.clone()),
            );
            iterable
        };
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, args| {
            *callback_observed.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        let completion = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (outer_binding, make_iterable("outer")),
                (inner_binding, make_iterable("inner")),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&completion),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(*observed.borrow(), Value::string("done"));
        assert_eq!(closed.borrow().as_slice(), &["inner", "outer"]);
    }

    #[test]
    fn labeled_continue_closes_inner_iterator_but_keeps_target_iterator_open() {
        let module = lower_script(
            r#"
                let seen = 0;
                outer: for (const a of outerIterable) {
                    for (const b of innerIterable) {
                        seen += a;
                        continue outer;
                    }
                }
                callback(seen);
            "#,
            "https://example.test/labeled-continue-close.js",
        )
        .unwrap();
        let outer_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "outerIterable")
            .unwrap()
            .local;
        let inner_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "innerIterable")
            .unwrap()
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;

        let outer_index = Rc::new(Cell::new(0_u32));
        let next_outer_index = Rc::clone(&outer_index);
        let outer_iterator = Value::object(HashMap::new());
        outer_iterator.set_property(
            "next",
            Value::function(move |_, _| {
                let index = next_outer_index.get();
                next_outer_index.set(index + 1);
                w3cos_core::js_object! {
                    "value" => Value::Number((index + 1) as f64),
                    "done" => Value::Bool(index >= 2),
                }
            }),
        );
        let outer_closed = Rc::new(Cell::new(0_u32));
        let outer_closed_by_return = Rc::clone(&outer_closed);
        outer_iterator.set_property(
            "return",
            Value::function(move |_, _| {
                outer_closed_by_return.set(outer_closed_by_return.get() + 1);
                w3cos_core::js_object! { "done" => Value::Bool(true) }
            }),
        );
        let outer_iterable = Value::object(HashMap::new());
        outer_iterable.set_property(
            "__w3cos_symbol_iterator",
            Value::function(move |_, _| outer_iterator.clone()),
        );

        let inner_closed = Rc::new(Cell::new(0_u32));
        let inner_closed_by_factory = Rc::clone(&inner_closed);
        let inner_iterable = Value::object(HashMap::new());
        inner_iterable.set_property(
            "__w3cos_symbol_iterator",
            Value::function(move |_, _| {
                let yielded = Rc::new(Cell::new(false));
                let next_yielded = Rc::clone(&yielded);
                let iterator = Value::object(HashMap::new());
                iterator.set_property(
                    "next",
                    Value::function(move |_, _| {
                        let done = next_yielded.replace(true);
                        w3cos_core::js_object! {
                            "value" => Value::Number(1.0),
                            "done" => Value::Bool(done),
                        }
                    }),
                );
                let inner_closed = Rc::clone(&inner_closed_by_factory);
                iterator.set_property(
                    "return",
                    Value::function(move |_, _| {
                        inner_closed.set(inner_closed.get() + 1);
                        w3cos_core::js_object! { "done" => Value::Bool(true) }
                    }),
                );
                iterator
            }),
        );
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, args| {
            *callback_observed.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (outer_binding, outer_iterable),
                (inner_binding, inner_iterable),
                (callback_binding, callback),
            ]))
            .unwrap();
        assert_eq!(*observed.borrow(), Value::Number(3.0));
        assert_eq!(inner_closed.get(), 2);
        assert_eq!(outer_closed.get(), 0);
    }

    #[test]
    fn for_let_refreshes_binding_cells_for_each_closure_iteration() {
        let module = lower_script(
            r#"
                let initializer;
                const callbacks = [];
                for (
                    let index = (initializer = () => index, 0);
                    index < 3;
                    index++
                ) {
                    callbacks[index] = () => index;
                    if (index === 1) {
                        continue;
                    }
                }
                callback(
                    initializer(),
                    callbacks[0](),
                    callbacks[1](),
                    callbacks[2]()
                );
            "#,
            "https://example.test/for-let-closures.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(2.0)
            ]
        );
    }

    #[test]
    fn for_in_reuses_shared_keys_and_iterator_cfg_for_all_head_forms() {
        let module = lower_script(
            r#"
                callback(typeof hoisted);
                for (var hoisted in { only: 1 }) {
                    callback("var:" + hoisted);
                }

                const readers = [];
                for (let key in { first: 1, second: 2, third: 3 }) {
                    readers.push(() => key);
                }
                for (const read of readers) {
                    callback("closure:" + read());
                }

                const holder = {};
                for (holder.key in { member: 1 }) {}
                callback("member:" + holder.key);

                var breakCount = 0;
                for (const key in { left: 1, right: 2 }) {
                    breakCount++;
                    break;
                }
                callback("break:" + breakCount);
            "#,
            "https://example.test/for-in.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    callback_observed
                        .borrow_mut()
                        .push(arguments[0].to_js_string());
                    Value::Undefined
                }),
            )]))
            .unwrap();
        let mut observed = observed.borrow().clone();
        observed.sort();
        assert_eq!(
            observed,
            vec![
                "break:1",
                "closure:first",
                "closure:second",
                "closure:third",
                "member:member",
                "undefined",
                "var:only",
            ]
        );
    }

    #[test]
    fn classes_use_shared_core_construction_prototypes_and_this_bindings() {
        let module = lower_script(
            r#"
                class Counter {
                    constructor(start) {
                        this.value = start;
                    }
                    add(delta) {
                        this.value += delta;
                        return this.value;
                    }
                    readWithArrow() {
                        return (() => this.value)();
                    }
                    static label() {
                        return "Counter";
                    }
                }
                const counter = new Counter(2);
                callback(counter.add(3));
                callback(counter.readWithArrow());
                callback(counter instanceof Counter);
                callback(Counter.label());

                const Base = class {
                    constructor(value) {
                        this.value = value;
                    }
                    read() {
                        return this.value;
                    }
                };
                class Child extends Base {
                    double() {
                        return this.value * 2;
                    }
                    parentRead() {
                        return super.read();
                    }
                    parentReadType() {
                        return typeof super.read;
                    }
                }
                const child = new Child(4);
                callback(child.read());
                callback(child.double());
                callback(child.parentRead());
                callback(child.parentReadType());
                callback(child instanceof Base);
                callback(child instanceof Child);

                class ExplicitChild extends Base {
                    constructor(value) {
                        super(value + 1);
                        this.extra = 2;
                    }
                    total() {
                        return this.value + this.extra;
                    }
                }
                const explicit = new ExplicitChild(5);
                callback(explicit.total());
                callback(explicit instanceof Base);

                const Named = class Internal {
                    static self() {
                        return Internal;
                    }
                };
                callback(Named.self() === Named);

                class AccessorBase {
                    constructor(value) {
                        this._value = value;
                    }
                    get value() {
                        return this._value * 2;
                    }
                    set value(next) {
                        this._value = next + 1;
                    }
                    static get title() {
                        return this.prefix;
                    }
                    static suffix() {
                        return "!";
                    }
                }
                class AccessorChild extends AccessorBase {
                    readAfterSet(next) {
                        this.value = next;
                        return this.value;
                    }
                    static summary() {
                        return super.title + super.suffix();
                    }
                    static arrowSummary() {
                        return (() => super.title)();
                    }
                }
                AccessorChild.prefix = "D";
                const accessor = new AccessorChild(1);
                callback(accessor.value);
                callback(accessor.readAfterSet(4));
                callback(AccessorChild.summary());
                callback(AccessorChild.arrowSummary());

                const accessorKey = "computed";
                class ComputedAccessor {
                    get [accessorKey]() {
                        return 7;
                    }
                }
                callback(new ComputedAccessor().computed);

                class SuperWriteBase {
                    get value() {
                        return this._value;
                    }
                    set value(next) {
                        this._value = next * 2;
                    }
                    static get count() {
                        return this._count;
                    }
                    static set count(next) {
                        this._count = next + 1;
                    }
                }
                class SuperWriteChild extends SuperWriteBase {
                    update() {
                        let keyReads = 0;
                        const key = () => {
                            keyReads += 1;
                            return "value";
                        };
                        const assigned = super[key()] = 3;
                        const post = super[key()]++;
                        const pre = ++super[key()];
                        super[key()] += 2;
                        super[key()] ||= 99;
                        return (
                            assigned + ":" + post + ":" + pre + ":" +
                            super[key()] + ":" + this._value + ":" + keyReads
                        );
                    }
                    static update() {
                        const assigned = super.count = 2;
                        super.count += 2;
                        const post = super.count++;
                        return assigned + ":" + post + ":" + this._count;
                    }
                }
                callback(new SuperWriteChild().update());
                callback(SuperWriteChild.update());
            "#,
            "https://example.test/classes.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    callback_observed
                        .borrow_mut()
                        .push(arguments.first().cloned().unwrap_or(Value::Undefined));
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(
            *observed.borrow(),
            vec![
                Value::Number(5.0),
                Value::Number(5.0),
                Value::Bool(true),
                Value::string("Counter"),
                Value::Number(4.0),
                Value::Number(8.0),
                Value::Number(4.0),
                Value::string("function"),
                Value::Bool(true),
                Value::Bool(true),
                Value::Number(8.0),
                Value::Bool(true),
                Value::Bool(true),
                Value::Number(2.0),
                Value::Number(10.0),
                Value::string("D!"),
                Value::string("D"),
                Value::Number(7.0),
                Value::string("3:6:15:64:64:6"),
                Value::string("2:6:8"),
            ]
        );
    }

    #[test]
    fn public_class_fields_use_shared_core_initializer_order() {
        let module = lower_script(
            r#"
                let fieldKey = "computed";
                class FieldBase {
                    base = callback("base-field");
                    value = 5;
                    [fieldKey] = 6;
                    static count = 3;
                    static {
                        let local = this.count;
                        callback("static-block:" + local);
                        this.count += 2;
                    }
                    static after = callback("static-after:" + this.count);
                    set value(next) {
                        callback("setter:" + next);
                    }
                    constructor() {
                        callback("base-ctor");
                    }
                }
                class FieldChild extends FieldBase {
                    child = callback("child-field");
                    after = this.computed + 1;
                    constructor() {
                        callback("before-super");
                        super();
                        callback("after-super");
                    }
                }
                class FieldGrandchild extends FieldChild {
                    grandchild = callback("grandchild-field");
                }
                class StaticParent {
                    static label() {
                        return "P";
                    }
                }
                class StaticChild extends StaticParent {
                    static {
                        callback("static-super:" + super.label());
                        this.mark = "C";
                        callback("static-this:" + this.mark);
                    }
                }

                fieldKey = "changed";
                callback("static:" + FieldBase.count);
                const fields = new FieldGrandchild();
                callback("value:" + fields.value);
                callback("computed:" + fields.computed);
                callback("after:" + fields.after);
            "#,
            "https://example.test/class-fields.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    callback_observed.borrow_mut().push(
                        arguments
                            .first()
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                    );
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(
            *observed.borrow(),
            vec![
                "static-block:3",
                "static-after:5",
                "static-super:P",
                "static-this:C",
                "static:5",
                "before-super",
                "base-field",
                "base-ctor",
                "child-field",
                "after-super",
                "grandchild-field",
                "value:5",
                "computed:6",
                "after:7",
            ]
        );
    }

    #[test]
    fn typescript_parameter_properties_follow_class_initializer_and_super_order() {
        let module = lower_module(
            r#"
                import { callback } from "host";
                class Base {
                    field = callback("base-field");
                    constructor(
                        public value: number = 2,
                        readonly label: string = "base"
                    ) {
                        callback("base-body:" + this.value);
                    }
                }
                class Child extends Base {
                    childField = callback("child-field");
                    constructor(value: number, public extra: number = 3) {
                        callback("before-super");
                        super(value, "child");
                        callback("after-super:" + this.extra);
                    }
                }
                const base = new Base();
                const child = new Child(5);
                callback(
                    base.value,
                    base.label,
                    child.value,
                    child.label,
                    child.extra
                );
            "#,
            "https://example.test/parameter-properties.ts",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    callback_observed.borrow_mut().extend(arguments);
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::string("base-field"),
                Value::string("base-body:2"),
                Value::string("before-super"),
                Value::string("base-field"),
                Value::string("base-body:5"),
                Value::string("child-field"),
                Value::string("after-super:3"),
                Value::Number(2.0),
                Value::string("base"),
                Value::Number(5.0),
                Value::string("child"),
                Value::Number(3.0),
            ]
        );
    }

    #[test]
    fn derived_parameter_properties_reject_unprovable_super_order() {
        let error = lower_script(
            r#"
                class Child extends Base {
                    constructor(public value: number) {
                        if (condition) super();
                    }
                }
            "#,
            "https://example.test/conditional-super.ts",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("require a direct super() statement"),
            "{error}"
        );
    }

    #[test]
    fn private_fields_use_shared_brands_and_hidden_core_slots() {
        let module = lower_script(
            r#"
                class Vault {
                    #value = 3;
                    static #count = 4;
                    #add(delta) { return this.#value + delta; }
                    get #double() { return this.#value * 2; }
                    set #double(next) { this.#value = next / 2; }
                    read() { return this.#value; }
                    write(next) {
                        this.#value += next;
                        this.#value++;
                    }
                    add(delta) { return this.#add(delta); }
                    readDouble() { return this.#double; }
                    writeDouble(next) { this.#double = next; }
                    has(value) { return #value in value; }
                    static readCount() { return this.#count; }
                }
                const first = new Vault();
                const second = new Vault();
                first.write(5);
                callback(first.read());
                callback(second.read());
                callback(first.add(2));
                callback(first.readDouble());
                first.writeDouble(20);
                callback(first.read());
                callback(first.has(first));
                callback(first.has({}));
                callback(Vault.readCount());
            "#,
            "https://example.test/private-fields.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    callback_observed
                        .borrow_mut()
                        .push(arguments.first().cloned().unwrap_or(Value::Undefined));
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(
            *observed.borrow(),
            vec![
                Value::Number(9.0),
                Value::Number(3.0),
                Value::Number(11.0),
                Value::Number(18.0),
                Value::Number(10.0),
                Value::Bool(true),
                Value::Bool(false),
                Value::Number(4.0),
            ]
        );
    }

    #[test]
    fn for_of_uses_shared_iterators_and_per_iteration_lexical_cells() {
        let module = lower_script(
            r#"
                const callbacks = [];
                let total = 0;
                for (const [index, value] of [[0, 2], [1, 3], [2, 4]]) {
                    callbacks.push(() => value);
                    if (index === 0) continue;
                    total += value;
                    if (index === 1) break;
                }

                var beforeItem = item;
                for (var item of [11]) {}

                let target = 0;
                for (target of [7]) {}
                const box = {};
                for (box.value of [8]) {}
                let first = 0;
                let second = 0;
                for ([first, second] of [[9, 10]]) {}

                let letters = "";
                for (const character of "A😀") {
                    letters += character;
                }
                let entries = "";
                for (const [key, entryValue] of collection) {
                    entries += key + entryValue;
                }
                let setTotal = 0;
                for (const setValue of setValues) {
                    setTotal += setValue;
                }
                for (const ignored of closable) {
                    break;
                }

                callback(
                    total,
                    callbacks[0](),
                    callbacks[1](),
                    beforeItem,
                    item,
                    target,
                    box.value,
                    first,
                    second,
                    letters,
                    entries,
                    setTotal
                );
            "#,
            "https://example.test/for-of.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let collection_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "collection")
            .unwrap()
            .local;
        let set_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "setValues")
            .unwrap()
            .local;
        let closable_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "closable")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });
        let collection = w3cos_core::class::construct(
            &w3cos_core::collections::map_class(),
            vec![Value::array(vec![
                Value::array(vec![Value::string("a"), Value::Number(1.0)]),
                Value::array(vec![Value::string("b"), Value::Number(2.0)]),
            ])],
        );
        let set_values = w3cos_core::class::construct(
            &w3cos_core::collections::set_class(),
            vec![Value::array(vec![Value::Number(3.0), Value::Number(4.0)])],
        );
        let iterator_index = Rc::new(Cell::new(0_u32));
        let next_index = Rc::clone(&iterator_index);
        let iterator = Value::object(HashMap::new());
        iterator.set_property(
            "next",
            Value::function(move |_, _| {
                let index = next_index.get();
                next_index.set(index + 1);
                if index == 0 {
                    w3cos_core::js_object! {
                        "value" => Value::Number(1.0),
                        "done" => Value::Bool(false),
                    }
                } else {
                    w3cos_core::js_object! {
                        "value" => Value::Undefined,
                        "done" => Value::Bool(true),
                    }
                }
            }),
        );
        let closed = Rc::new(Cell::new(0_u32));
        let closed_by_return = Rc::clone(&closed);
        iterator.set_property(
            "return",
            Value::function(move |_, _| {
                closed_by_return.set(closed_by_return.get() + 1);
                w3cos_core::js_object! { "done" => Value::Bool(true) }
            }),
        );
        let iterable = Value::object(HashMap::new());
        let custom_iterator = iterator.clone();
        iterable.set_property(
            "__w3cos_symbol_iterator",
            Value::function(move |_, _| custom_iterator.clone()),
        );

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (callback_binding, callback),
                (collection_binding, collection),
                (set_binding, set_values),
                (closable_binding, iterable),
            ]))
            .unwrap();
        assert_eq!(closed.get(), 1);
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(3.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Undefined,
                Value::Number(11.0),
                Value::Number(7.0),
                Value::Number(8.0),
                Value::Number(9.0),
                Value::Number(10.0),
                Value::string("A😀"),
                Value::string("a1b2"),
                Value::Number(7.0),
            ]
        );
    }

    #[test]
    fn for_of_over_map_observes_live_mutation_through_shared_core_iterator() {
        let module = lower_script(
            r#"
                let seen = "";
                for (const [key, value] of collection) {
                    seen += key;
                    if (value === 1) {
                        collection.delete("b");
                        collection.set("c", 3);
                        collection.delete("a");
                        collection.set("a", 4);
                    }
                }
                callback(seen);
            "#,
            "https://example.test/for-of-live-map.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let collection_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "collection")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });
        let collection = w3cos_core::class::construct(
            &w3cos_core::collections::map_class(),
            vec![Value::array(vec![
                Value::array(vec![Value::string("a"), Value::Number(1.0)]),
                Value::array(vec![Value::string("b"), Value::Number(2.0)]),
            ])],
        );

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (callback_binding, callback),
                (collection_binding, collection),
            ]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &[Value::string("aca")]);
    }

    #[test]
    fn for_of_over_array_observes_live_length_through_shared_core_iterator() {
        let module = lower_script(
            r#"
                const values = [1, 2];
                let seen = "";
                for (const value of values) {
                    seen += value;
                    if (value === 1) {
                        values.pop();
                        values.push(3);
                        values.push(4);
                    }
                }
                callback(seen);
            "#,
            "https://example.test/for-of-live-array.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &[Value::string("134")]);
    }

    #[test]
    fn for_of_over_typed_array_reads_live_values_from_shared_backing() {
        let module = lower_script(
            r#"
                let seen = "";
                for (const value of typed) {
                    seen += value;
                    if (value === 1) {
                        typed.set([9], 1);
                    }
                }
                callback(seen);
            "#,
            "https://example.test/for-of-live-typed-array.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let typed_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "typed")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });
        let typed =
            w3cos_core::binary::typed_array_value(vec![Value::Number(1.0), Value::Number(2.0)]);

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (callback_binding, callback),
                (typed_binding, typed),
            ]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &[Value::string("19")]);
    }

    #[test]
    fn for_await_of_uses_async_protocol_awaits_values_and_closes_on_break() {
        let module = lower_module(
            r#"
                let seen = "";
                for await (const value of iterable) {
                    seen += value;
                    if (value === 2) break;
                }
                callback(seen);
            "#,
            "https://example.test/for-await-of.js",
        )
        .unwrap();
        let iterable_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "iterable")
            .unwrap()
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;

        let index = Rc::new(Cell::new(0_u32));
        let next_index = Rc::clone(&index);
        let closed = Rc::new(Cell::new(0_u32));
        let closed_by_return = Rc::clone(&closed);
        let iterator = Value::object(HashMap::new());
        iterator.set_property(
            "next",
            Value::function(move |_, _| {
                let index = next_index.get() + 1;
                next_index.set(index);
                let step = if index <= 3 {
                    w3cos_core::js_object! {
                        "value" => w3cos_core::promise::resolve(vec![Value::Number(index as f64)]),
                        "done" => Value::Bool(false),
                    }
                } else {
                    w3cos_core::js_object! {
                        "value" => Value::Undefined,
                        "done" => Value::Bool(true),
                    }
                };
                w3cos_core::promise::resolve(vec![step])
            }),
        );
        iterator.set_property(
            "return",
            Value::function(move |_, _| {
                closed_by_return.set(closed_by_return.get() + 1);
                w3cos_core::promise::resolve(vec![
                    w3cos_core::js_object! { "done" => Value::Bool(true) },
                ])
            }),
        );
        let iterable = Value::object(HashMap::new());
        let async_iterator = iterator.clone();
        iterable.set_property(
            "__w3cos_symbol_async_iterator",
            Value::function(move |_, _| async_iterator.clone()),
        );

        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, args| {
            *callback_observed.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        let completion = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (iterable_binding, iterable),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&completion),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(*observed.borrow(), Value::string("12"));
        assert_eq!(closed.get(), 1);
    }

    #[test]
    fn for_await_of_falls_back_to_sync_iterables_and_awaits_their_values() {
        let module = lower_module(
            r#"
                let seen = "";
                for await (const value of iterable) seen += value;
                callback(seen);
            "#,
            "https://example.test/for-await-sync-fallback.js",
        )
        .unwrap();
        let iterable_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "iterable")
            .unwrap()
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, args| {
            *callback_observed.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        let iterable = Value::array(vec![
            w3cos_core::promise::resolve(vec![Value::Number(4.0)]),
            Value::Number(5.0),
        ]);
        let completion = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (iterable_binding, iterable),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&completion),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(*observed.borrow(), Value::string("45"));
    }

    #[test]
    fn for_await_of_async_close_preserves_an_existing_throw() {
        let module = lower_module(
            r#"
                for await (const value of iterable) {
                    throw failure;
                }
            "#,
            "https://example.test/for-await-close-throw.js",
        )
        .unwrap();
        let iterable_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "iterable")
            .unwrap()
            .local;
        let failure_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "failure")
            .unwrap()
            .local;

        let yielded = Rc::new(Cell::new(false));
        let next_yielded = Rc::clone(&yielded);
        let closed = Rc::new(Cell::new(0_u32));
        let closed_by_return = Rc::clone(&closed);
        let iterator = Value::object(HashMap::new());
        iterator.set_property(
            "next",
            Value::function(move |_, _| {
                let done = next_yielded.replace(true);
                w3cos_core::promise::resolve(vec![w3cos_core::js_object! {
                    "value" => Value::Number(1.0),
                    "done" => Value::Bool(done),
                }])
            }),
        );
        iterator.set_property(
            "return",
            Value::function(move |_, _| {
                closed_by_return.set(closed_by_return.get() + 1);
                w3cos_core::promise::reject(vec![Value::string("close failure")])
            }),
        );
        let iterable = Value::object(HashMap::new());
        let async_iterator = iterator.clone();
        iterable.set_property(
            "__w3cos_symbol_async_iterator",
            Value::function(move |_, _| async_iterator.clone()),
        );
        let failure = Value::string("body failure");
        let completion = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (iterable_binding, iterable),
                (failure_binding, failure.clone()),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&completion),
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason))
                if reason.strict_eq(&failure)
        ));
        assert_eq!(closed.get(), 1);
    }

    #[test]
    fn for_await_of_return_waits_for_async_iterator_close() {
        let module = lower_module(
            r#"
                async function first() {
                    for await (const value of iterable) return value;
                }
                callback(await first());
            "#,
            "https://example.test/for-await-return.js",
        )
        .unwrap();
        let iterable_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "iterable")
            .unwrap()
            .local;
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;

        let iterator = Value::object(HashMap::new());
        iterator.set_property(
            "next",
            Value::function(|_, _| {
                w3cos_core::promise::resolve(vec![w3cos_core::js_object! {
                    "value" => Value::Number(7.0),
                    "done" => Value::Bool(false),
                }])
            }),
        );
        let closed = Rc::new(Cell::new(0_u32));
        let closed_by_return = Rc::clone(&closed);
        iterator.set_property(
            "return",
            Value::function(move |_, _| {
                closed_by_return.set(closed_by_return.get() + 1);
                w3cos_core::promise::resolve(vec![
                    w3cos_core::js_object! { "done" => Value::Bool(true) },
                ])
            }),
        );
        let iterable = Value::object(HashMap::new());
        let async_iterator = iterator.clone();
        iterable.set_property(
            "__w3cos_symbol_async_iterator",
            Value::function(move |_, _| async_iterator.clone()),
        );
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, args| {
            *callback_observed.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        let completion = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (iterable_binding, iterable),
                (callback_binding, callback),
            ]))
            .unwrap();
        w3cos_core::promise::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&completion),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(*observed.borrow(), Value::Number(7.0));
        assert_eq!(closed.get(), 1);
    }

    #[test]
    fn for_of_closes_nested_iterators_for_return_and_preserves_explicit_throw() {
        let module = lower_script(
            r#"
                function exitNested() {
                    for (const outerValue of outerIterable) {
                        for (const innerValue of innerIterable) {
                            return outerValue + innerValue;
                        }
                    }
                }
                callback(exitNested());
                for (const value of throwingIterable) {
                    throw "original throw";
                }
            "#,
            "https://example.test/for-of-abrupt.js",
        )
        .unwrap();
        let binding = |name: &str| {
            module
                .imports
                .iter()
                .find(|import| import.imported == name)
                .unwrap()
                .local
        };
        let callback_binding = binding("callback");
        let outer_binding = binding("outerIterable");
        let inner_binding = binding("innerIterable");
        let throwing_binding = binding("throwingIterable");
        let closed = Rc::new(RefCell::new(Vec::new()));
        let make_iterable = |name: &'static str, value: f64, throws_on_close: bool| {
            let index = Rc::new(Cell::new(0_u32));
            let next_index = Rc::clone(&index);
            let iterator = Value::object(HashMap::new());
            iterator.set_property(
                "next",
                Value::function(move |_, _| {
                    let current = next_index.get();
                    next_index.set(current + 1);
                    w3cos_core::js_object! {
                        "value" => if current == 0 {
                            Value::Number(value)
                        } else {
                            Value::Undefined
                        },
                        "done" => Value::Bool(current != 0),
                    }
                }),
            );
            let close_log = Rc::clone(&closed);
            iterator.set_property(
                "return",
                Value::function(move |_, _| {
                    close_log.borrow_mut().push(name);
                    if throws_on_close {
                        w3cos_core::throw_value(Value::string("close failure"));
                    }
                    w3cos_core::js_object! { "done" => Value::Bool(true) }
                }),
            );
            let iterable = Value::object(HashMap::new());
            iterable.set_property(
                "__w3cos_symbol_iterator",
                Value::function(move |_, _| iterator.clone()),
            );
            iterable
        };
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() =
                arguments.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        });
        let result = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (callback_binding, callback),
                (outer_binding, make_iterable("outer", 10.0, false)),
                (inner_binding, make_iterable("inner", 2.0, false)),
                (throwing_binding, make_iterable("throwing", 1.0, true)),
            ]));

        assert_eq!(*observed.borrow(), Value::Number(12.0));
        assert_eq!(closed.borrow().as_slice(), &["inner", "outer", "throwing"]);
        match result {
            Err(VmError::Thrown(value)) => assert_eq!(value, Value::string("original throw")),
            other => panic!("expected original explicit throw, got {other:?}"),
        }
    }

    #[test]
    fn for_of_closes_nested_iterators_for_host_call_exceptions() {
        let module = lower_script(
            r#"
                function invoke() {
                    for (const outerValue of outerIterable) {
                        for (const innerValue of innerIterable) {
                            fail();
                        }
                    }
                }
                invoke();
            "#,
            "https://example.test/for-of-host-throw.js",
        )
        .unwrap();
        let binding = |name: &str| {
            module
                .imports
                .iter()
                .find(|import| import.imported == name)
                .unwrap()
                .local
        };
        let outer_binding = binding("outerIterable");
        let inner_binding = binding("innerIterable");
        let fail_binding = binding("fail");
        let closed = Rc::new(RefCell::new(Vec::new()));
        let make_iterable = |name: &'static str, throws_on_close: bool| {
            let iterator = Value::object(HashMap::new());
            iterator.set_property(
                "next",
                Value::function(|_, _| {
                    w3cos_core::js_object! {
                        "value" => Value::Number(1.0),
                        "done" => Value::Bool(false),
                    }
                }),
            );
            let close_log = Rc::clone(&closed);
            iterator.set_property(
                "return",
                Value::function(move |_, _| {
                    close_log.borrow_mut().push(name);
                    if throws_on_close {
                        w3cos_core::throw_value(Value::string("close failure"));
                    }
                    w3cos_core::js_object! { "done" => Value::Bool(true) }
                }),
            );
            let iterable = Value::object(HashMap::new());
            iterable.set_property(
                "__w3cos_symbol_iterator",
                Value::function(move |_, _| iterator.clone()),
            );
            iterable
        };
        let fail = Value::function(|_, _| w3cos_core::throw_value(Value::string("host failure")));
        let result = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (outer_binding, make_iterable("outer", false)),
                (inner_binding, make_iterable("inner", true)),
                (fail_binding, fail),
            ]));

        assert_eq!(closed.borrow().as_slice(), &["inner", "outer"]);
        match result {
            Err(VmError::Thrown(value)) => assert_eq!(value, Value::string("host failure")),
            other => panic!("expected original Host-call exception, got {other:?}"),
        }
    }

    #[test]
    fn for_of_rejects_malformed_next_and_return_protocols() {
        let malformed_next_module = lower_script(
            r#"
                for (const value of malformedNext) {
                    callback(value);
                }
            "#,
            "https://example.test/for-of-malformed-next.js",
        )
        .unwrap();
        let next_binding = malformed_next_module
            .imports
            .iter()
            .find(|import| import.imported == "malformedNext")
            .unwrap()
            .local;
        let callback_binding = malformed_next_module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let closed = Rc::new(Cell::new(0_u32));
        let iterator = Value::object(HashMap::new());
        iterator.set_property("next", Value::function(|_, _| Value::Number(1.0)));
        let close_count = Rc::clone(&closed);
        iterator.set_property(
            "return",
            Value::function(move |_, _| {
                close_count.set(close_count.get() + 1);
                w3cos_core::js_object! { "done" => Value::Bool(true) }
            }),
        );
        let malformed_next = Value::object(HashMap::new());
        malformed_next.set_property(
            "__w3cos_symbol_iterator",
            Value::function(move |_, _| iterator.clone()),
        );
        let next_result = Vm::new(malformed_next_module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (next_binding, malformed_next),
                (
                    callback_binding,
                    Value::function(|_, _| panic!("malformed next must not enter loop body")),
                ),
            ]));
        assert_eq!(closed.get(), 0, "IteratorStep failure must not close");
        match next_result {
            Err(VmError::Thrown(value)) => {
                assert_eq!(value.get_property("name"), Value::string("TypeError"))
            }
            other => panic!("expected malformed next TypeError, got {other:?}"),
        }

        let malformed_return_module = lower_script(
            r#"
                for (const value of malformedReturn) {
                    fail();
                }
            "#,
            "https://example.test/for-of-malformed-return.js",
        )
        .unwrap();
        let return_binding = malformed_return_module
            .imports
            .iter()
            .find(|import| import.imported == "malformedReturn")
            .unwrap()
            .local;
        let fail_binding = malformed_return_module
            .imports
            .iter()
            .find(|import| import.imported == "fail")
            .unwrap()
            .local;
        let iterator = Value::object(HashMap::new());
        iterator.set_property(
            "next",
            Value::function(|_, _| {
                w3cos_core::js_object! {
                    "value" => Value::Number(1.0),
                    "done" => Value::Bool(false),
                }
            }),
        );
        iterator.set_property("return", Value::Number(1.0));
        let malformed_return = Value::object(HashMap::new());
        malformed_return.set_property(
            "__w3cos_symbol_iterator",
            Value::function(move |_, _| iterator.clone()),
        );
        let return_result = Vm::new(malformed_return_module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([
                (return_binding, malformed_return),
                (
                    fail_binding,
                    Value::function(|_, _| {
                        w3cos_core::throw_value(Value::string("original host failure"))
                    }),
                ),
            ]));
        match return_result {
            Err(VmError::Thrown(value)) => {
                assert_eq!(value.get_property("name"), Value::string("TypeError"))
            }
            other => panic!("expected malformed return TypeError, got {other:?}"),
        }
    }

    #[test]
    fn lexical_blocks_shadow_without_leaking_bindings() {
        let module = lower_script(
            r#"
                let value = "outer";
                if (true) {
                    let value = "inner";
                    callback(value);
                }
                callback(value);
            "#,
            "https://example.test/block-scope.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed
                .borrow_mut()
                .push(arguments[0].to_js_string());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &["inner", "outer"]);
    }

    #[test]
    fn repeated_block_entries_refresh_lexical_cells_for_closures() {
        let module = lower_script(
            r#"
                const callbacks = [];
                var index = 0;
                while (index < 3) {
                    callbacks.push(() => value);
                    let value = index;
                    index++;
                }
                callback(
                    callbacks[0](),
                    callbacks[1](),
                    callbacks[2]()
                );
            "#,
            "https://example.test/repeated-block-cells.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[Value::Number(0.0), Value::Number(1.0), Value::Number(2.0)]
        );
    }

    #[test]
    fn block_function_declarations_hoist_and_remain_block_scoped() {
        let module = lower_script(
            r#"
                let describe = () => "outer";
                {
                    callback(describe());
                    function describe() {
                        return "inner";
                    }
                }
                switch (1) {
                    case 1:
                        callback(describe());
                        function describe() {
                            return "switch";
                        }
                        break;
                }
                callback(describe());
            "#,
            "https://example.test/block-function-hoist.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed
                .borrow_mut()
                .push(arguments[0].to_js_string());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &["inner", "switch", "outer"]);
    }

    #[test]
    fn lexical_binding_is_predeclared_and_uses_tdz_initialization() {
        let module = lower_script(
            r#"
                callback(value);
                let value = "ready";
            "#,
            "https://example.test/lexical-tdz.js",
        )
        .unwrap();
        let entry = &module.functions[0];
        let value_binding = entry
            .bindings
            .iter()
            .find(|binding| binding.name == "value")
            .unwrap();
        assert_eq!(value_binding.kind, BindingKind::Let);
        let value_binding_id = value_binding.id;
        assert!(entry.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::InitializeBinding { binding, .. }
                        if *binding == value_binding_id
                )
            })
        }));

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let error = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(|_, _| Value::Undefined),
            )]))
            .unwrap_err();
        assert!(matches!(
            error,
            w3cos_vm::VmError::ReferenceError(message)
                if message.contains("'value'") && message.contains("before initialization")
        ));
    }

    #[test]
    fn var_binding_is_function_scoped_and_hoisted_as_undefined() {
        let module = lower_script(
            r#"
                callback(value);
                if (true) {
                    var value = "ready";
                }
                callback(value);
            "#,
            "https://example.test/var-hoist.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments[0].clone());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert!(observed.borrow()[0].is_undefined());
        assert_eq!(observed.borrow()[1], Value::string("ready"));
    }

    #[test]
    fn var_binding_inside_label_is_function_scoped() {
        let module = lower_script(
            r#"
                outer: {
                    var value = "ready";
                    break outer;
                }
                callback(value);
            "#,
            "https://example.test/labeled-var-hoist.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments[0].clone());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &[Value::string("ready")]);
    }

    #[test]
    fn ordinary_functions_bind_arguments_and_arrows_capture_it() {
        let module = lower_script(
            r#"
                function read(value) {
                    const inner = () => arguments[0];
                    return inner();
                }
                callback(read("ready"));
            "#,
            "https://example.test/function-arguments.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments[0].clone());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &[Value::string("ready")]);
    }

    #[test]
    fn function_declarations_hoist_and_default_parameters_execute_in_w3vm() {
        let module = lower_script(
            r#"
                callback(add(2));
                callback(add(2, 4));
                function add(value, step = 3) {
                    return value + step;
                }
                const run = () => nested();
                callback(run());
                function nested() {
                    return "nested";
                }
            "#,
            "https://example.test/function-hoist.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed.borrow_mut().push(arguments[0].clone());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(5.0),
                Value::Number(6.0),
                Value::string("nested")
            ]
        );
    }

    #[test]
    fn annex_b_branch_functions_hoist_as_var_and_initialize_only_when_selected() {
        let module = lower_script(
            r#"
                callback(typeof selected);
                callback(typeof skipped);
                if (false) function skipped() {
                    return "skipped";
                }
                if (true) function selected() {
                    return "selected";
                }
                callback(selected());
                callback(typeof skipped);

                if (false) function choice() {
                    return "then";
                } else function choice() {
                    return "else";
                }
                callback(choice());

                function nested(enabled) {
                    callback(typeof local);
                    if (enabled) function local() {
                        return "nested";
                    }
                    callback(enabled ? local() : typeof local);
                }
                nested(true);
                nested(false);
            "#,
            "https://example.test/annex-b-branch-functions.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed
                .borrow_mut()
                .push(arguments[0].to_js_string());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                "undefined",
                "undefined",
                "selected",
                "undefined",
                "else",
                "undefined",
                "nested",
                "undefined",
                "undefined",
            ]
        );
    }

    #[test]
    fn annex_b_branch_functions_are_disabled_in_strict_scripts_and_modules() {
        let strict_error = lower_script(
            r#"
                "use strict";
                if (true) function strictBranch() {}
            "#,
            "https://example.test/strict-branch-function.js",
        )
        .unwrap_err();
        assert!(
            strict_error.to_string().contains("Annex B"),
            "{strict_error:#}"
        );

        let nested_strict_error = lower_script(
            r#"
                function strictNested() {
                    "use strict";
                    if (true) function strictBranch() {}
                }
            "#,
            "https://example.test/nested-strict-branch-function.js",
        )
        .unwrap_err();
        assert!(
            nested_strict_error.to_string().contains("Annex B"),
            "{nested_strict_error:#}"
        );

        let module_error = lower_module(
            "if (true) function moduleBranch() {}",
            "https://example.test/branch-function.mjs",
        )
        .unwrap_err();
        assert!(
            module_error.to_string().contains("Annex B"),
            "{module_error:#}"
        );
    }

    #[test]
    fn exported_function_declaration_uses_the_shared_w3ir_module_path() {
        let module = lower_module(
            r#"
                export function classify(value = "default") {
                    return value;
                }
            "#,
            "https://example.test/functions.mjs",
        )
        .unwrap();

        let exported = module
            .exports
            .iter()
            .find(|export| export.exported == "classify")
            .expect("function export");
        assert_eq!(
            module.functions[0]
                .bindings
                .iter()
                .find(|binding| binding.id == exported.local)
                .map(|binding| binding.name.as_str()),
            Some("classify")
        );
    }

    #[test]
    fn exported_destructuring_declares_each_live_module_binding() {
        let module = lower_module(
            r#"
                export const {
                    first,
                    nested: [second],
                    ...rest
                } = {
                    first: 1,
                    nested: [2],
                    third: 3
                };
                callback(first, second, rest.third);
            "#,
            "https://example.test/destructured-exports.mjs",
        )
        .unwrap();
        let exported = module
            .exports
            .iter()
            .map(|export| export.exported.as_str())
            .collect::<Vec<_>>();
        assert_eq!(exported, ["first", "second", "rest"]);

        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)]
        );
    }

    #[test]
    fn destructured_default_and_rest_parameters_execute_in_w3vm() {
        let module = lower_script(
            r#"
                function inspect(
                    { label = "fallback", nested: { count }, ...metadata },
                    [first, , third, ...tail],
                    ...rest
                ) {
                    return label + ":" + count + ":" + first + ":" +
                        third + ":" + metadata.extra + ":" + tail.length +
                        ":" + tail[0] + ":" + rest.length + ":" + rest[0];
                }
                function whole({ label } = { label: "whole" }) {
                    return label;
                }
                callback(
                    inspect(
                        { nested: { count: 2 }, extra: "meta" },
                        [3, 4, 5, 6, 7],
                        "tail",
                        "extra"
                    )
                );
                callback(whole());
            "#,
            "https://example.test/parameter-patterns.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            callback_observed
                .borrow_mut()
                .push(arguments[0].to_js_string());
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &["fallback:2:3:5:meta:2:6:2:tail", "whole"]
        );
    }

    #[test]
    fn parameter_defaults_initialize_left_to_right_and_preserve_later_tdz() {
        let ordered = lower_script(
            r#"
                function inspect(
                    first = "first",
                    second = first,
                    { third = second } = {},
                    ...tail
                ) {
                    callback(first + ":" + second + ":" + third + ":" + tail.length);
                }
                inspect();
                function capture(read = () => later, later = "late") {
                    callback(read());
                }
                capture();
            "#,
            "https://example.test/ordered-parameter-defaults.js",
        )
        .unwrap();
        let callback_binding = ordered
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        Vm::new(ordered, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    callback_observed
                        .borrow_mut()
                        .push(arguments[0].to_js_string());
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &["first:first:first:0", "late"]
        );

        for (source, specifier) in [
            (
                "function inspect(first = later, later = 'ready') {} inspect();",
                "https://example.test/later-parameter-read.js",
            ),
            (
                "function inspect(first = (later = 'write'), later) {} inspect();",
                "https://example.test/later-parameter-write.js",
            ),
            (
                "function inspect({ value = later } = {}, later = 'ready') {} inspect();",
                "https://example.test/destructured-later-parameter-read.js",
            ),
        ] {
            let error = Vm::new(lower_script(source, specifier).unwrap(), Limits::default())
                .unwrap()
                .run()
                .unwrap_err();
            let message = match &error {
                w3cos_vm::VmError::ReferenceError(message) => message.clone(),
                w3cos_vm::VmError::Thrown(value) => value.to_js_string(),
                _ => String::new(),
            };
            assert!(
                message.contains("'later'") && message.contains("before initialization"),
                "{specifier}: {error:?}"
            );
        }
    }

    #[test]
    fn destructured_variable_declarations_reuse_shared_pattern_intrinsics() {
        let module = lower_script(
            r#"
                const source = {
                    head: [1, void 0, 3, 4],
                    keep: "yes",
                    drop: "no"
                };
                let {
                    head: [first, second = 2, ...tail],
                    keep: renamed,
                    ...metadata
                } = source;
                var [varHead, ...varTail] = [5, 6, 7];

                const loopCallbacks = [];
                for (let [index] = [0]; index < 2; index++) {
                    loopCallbacks.push(() => index);
                }

                callback(
                    first,
                    second,
                    tail.join(":"),
                    renamed,
                    metadata.drop,
                    varHead,
                    varTail.join(":"),
                    loopCallbacks[0](),
                    loopCallbacks[1]()
                );
            "#,
            "https://example.test/destructured-declarations.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(1.0),
                Value::Number(2.0),
                Value::string("3:4"),
                Value::string("yes"),
                Value::string("no"),
                Value::Number(5.0),
                Value::string("6:7"),
                Value::Number(0.0),
                Value::Number(1.0),
            ]
        );
    }

    #[test]
    fn destructuring_reassignment_reuses_shared_w3ir_pattern_writes() {
        let module = lower_script(
            r#"
                let first = 0;
                let second = 0;
                let tail = [];
                const target = {};
                let targetKeyCalls = 0;
                function targetKey() {
                    targetKeyCalls += 1;
                    return "slot";
                }

                const arraySource = [1, void 0, 3, 4, 5];
                const arrayResult = (
                    [first, second = 2, target[targetKey()], ...tail] =
                        arraySource
                );

                let x = 0;
                let z = 0;
                let metadata = {};
                const objectSource = {
                    x: 6,
                    nested: { z: 7 },
                    keep: 8
                };
                const objectResult = (
                    { x, nested: { z }, ...metadata } = objectSource
                );

                let picked = 0;
                let sourceKeyCalls = 0;
                function sourceKey() {
                    sourceKeyCalls += 1;
                    return "answer";
                }
                ({ [sourceKey()]: picked } = { answer: 9 });

                class PrivateSink {
                    #value = 0;
                    assign(source) {
                        [this.#value] = source;
                        return this.#value;
                    }
                }

                callback(
                    first,
                    second,
                    target.slot,
                    tail.join(":"),
                    arrayResult === arraySource,
                    x,
                    z,
                    metadata.keep,
                    objectResult === objectSource,
                    picked,
                    targetKeyCalls,
                    sourceKeyCalls,
                    new PrivateSink().assign([10])
                );
            "#,
            "https://example.test/destructuring-reassignment.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments;
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::string("4:5"),
                Value::Bool(true),
                Value::Number(6.0),
                Value::Number(7.0),
                Value::Number(8.0),
                Value::Bool(true),
                Value::Number(9.0),
                Value::Number(1.0),
                Value::Number(1.0),
                Value::Number(10.0),
            ]
        );
    }

    #[test]
    fn destructuring_defaults_observe_left_to_right_tdz() {
        let module = lower_script(
            r#"
                let { first = later, later = 2 } = {};
                callback(first, later);
            "#,
            "https://example.test/destructuring-tdz.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let error = Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(|_, _| Value::Undefined),
            )]))
            .unwrap_err();
        assert!(matches!(
            error,
            w3cos_vm::VmError::ReferenceError(message)
                if message.contains("'later'") && message.contains("before initialization")
        ));
    }

    #[test]
    fn var_declaration_without_initializer_does_not_overwrite_earlier_assignment() {
        let module = lower_script(
            r#"
                value = "ready";
                var value;
                callback(value);
            "#,
            "https://example.test/var-no-initializer.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments[0].clone();
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(*observed.borrow(), Value::string("ready"));
    }

    #[test]
    fn structured_literals_and_locals_execute_in_w3vm() {
        let module = lower_script(
            r#"
                const status = "ready";
                let payload = { status, tiles: [1, 2] };
                payload.status = status + "!";
                payload.count = "6" / 2;
                payload.loose = "1" == 1;
                payload.strict = "1" === 1;
                callback(payload);
            "#,
            "https://example.test/jsonp.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments[0].clone();
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        let payload = observed.borrow().clone();
        assert_eq!(payload.get_property("status"), Value::string("ready!"));
        assert_eq!(
            payload.get_property("tiles").get_property("1"),
            Value::Number(2.0)
        );
        assert_eq!(payload.get_property("count"), Value::Number(3.0));
        assert_eq!(payload.get_property("loose"), Value::Bool(true));
        assert_eq!(payload.get_property("strict"), Value::Bool(false));
    }

    #[test]
    fn debugger_statement_is_a_backend_neutral_noop() {
        let module = lower_script(
            r#"
                let value = 1;
                debugger;
                value += 2;
                callback(value);
            "#,
            "https://example.test/debugger.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(Value::Undefined));
        let callback_observed = Rc::clone(&observed);
        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(
                callback_binding,
                Value::function(move |_, arguments| {
                    *callback_observed.borrow_mut() =
                        arguments.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                }),
            )]))
            .unwrap();
        assert_eq!(*observed.borrow(), Value::Number(3.0));
    }

    #[test]
    fn nested_arrow_callbacks_capture_live_outer_bindings() {
        let module = lower_script(
            r#"
                let suffix = "!";
                const makeHandler = (prefix) => (value) =>
                    callback(prefix + value + suffix);
                const handler = makeHandler("tile:");
                suffix = "?";
                handler("ready");
            "#,
            "https://example.test/callback.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(String::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments[0].to_js_string();
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_str(), "tile:ready?");
    }

    #[test]
    fn anonymous_function_expression_uses_the_shared_callable_abi() {
        let module = lower_script(
            r#"
                const handler = function(value) {
                    return callback("function:" + value);
                };
                handler("ready");
            "#,
            "https://example.test/function-expression.js",
        )
        .unwrap();
        let callback_binding = module
            .imports
            .iter()
            .find(|import| import.imported == "callback")
            .unwrap()
            .local;
        let observed = Rc::new(RefCell::new(String::new()));
        let callback_observed = Rc::clone(&observed);
        let callback = Value::function(move |_, arguments| {
            *callback_observed.borrow_mut() = arguments[0].to_js_string();
            Value::Undefined
        });

        Vm::new(module, Limits::default())
            .unwrap()
            .run_with_bindings(HashMap::from([(callback_binding, callback)]))
            .unwrap();
        assert_eq!(observed.borrow().as_str(), "function:ready");
    }
}
