/// TypeScript type → Rust type mapping.
///
/// Covers the core TS types that have natural Rust equivalents.
/// Union types are mapped to enums, nullable types to Option<T>.

#[derive(Debug, Clone, PartialEq)]
pub enum RustType {
    F64,
    String,
    Bool,
    Vec(Box<RustType>),
    Option(Box<RustType>),
    HashMap(Box<RustType>, Box<RustType>),
    Struct(String),
    Tuple(Vec<RustType>),
    Fn {
        params: Vec<RustType>,
        ret: Box<RustType>,
    },
    FnMut {
        params: Vec<RustType>,
        ret: Box<RustType>,
    },
    Rc(Box<RustType>),
    RcRefCell(Box<RustType>),
    BoxType(Box<RustType>),
    Future(Box<RustType>),
    Void,
    /// Fallback for unresolved / `any` types — a runtime-tagged value.
    Dynamic,
    Inferred,
}

impl RustType {
    pub fn to_rust_str(&self) -> String {
        match self {
            RustType::F64 => "f64".into(),
            RustType::String => "String".into(),
            RustType::Bool => "bool".into(),
            RustType::Vec(inner) => format!("Vec<{}>", inner.to_rust_str()),
            RustType::Option(inner) => format!("Option<{}>", inner.to_rust_str()),
            RustType::HashMap(k, v) => {
                format!("HashMap<{}, {}>", k.to_rust_str(), v.to_rust_str())
            }
            RustType::Struct(name) => name.clone(),
            RustType::Tuple(elems) => {
                let parts: Vec<_> = elems.iter().map(|t| t.to_rust_str()).collect();
                format!("({})", parts.join(", "))
            }
            RustType::Fn { params, ret } => {
                let ps: Vec<_> = params.iter().map(|t| t.to_rust_str()).collect();
                format!("Box<dyn Fn({}) -> {}>", ps.join(", "), ret.to_rust_str())
            }
            RustType::FnMut { params, ret } => {
                let ps: Vec<_> = params.iter().map(|t| t.to_rust_str()).collect();
                format!("Box<dyn FnMut({}) -> {}>", ps.join(", "), ret.to_rust_str())
            }
            RustType::Rc(inner) => format!("Rc<{}>", inner.to_rust_str()),
            RustType::RcRefCell(inner) => format!("Rc<RefCell<{}>>", inner.to_rust_str()),
            RustType::BoxType(inner) => format!("Box<{}>", inner.to_rust_str()),
            RustType::Future(inner) => format!("impl Future<Output = {}>", inner.to_rust_str()),
            RustType::Void => "()".into(),
            RustType::Dynamic => "Value".into(),
            RustType::Inferred => "_".into(),
        }
    }

    pub fn needs_hashmap_import(&self) -> bool {
        match self {
            RustType::HashMap(_, _) => true,
            RustType::Vec(inner) | RustType::Option(inner) => inner.needs_hashmap_import(),
            _ => false,
        }
    }

    pub fn needs_rc_import(&self) -> bool {
        match self {
            RustType::Rc(_) | RustType::RcRefCell(_) => true,
            RustType::Vec(inner) | RustType::Option(inner) | RustType::BoxType(inner) => {
                inner.needs_rc_import()
            }
            _ => false,
        }
    }
}

/// Resolve a TypeScript type annotation (from SWC AST) to a RustType.
pub fn resolve_ts_type(ts_type: &swc_ecma_ast::TsType) -> RustType {
    use swc_ecma_ast::TsType::*;

    match ts_type {
        TsKeywordType(kw) => resolve_keyword(kw),
        TsArrayType(arr) => {
            let inner = resolve_ts_type(&arr.elem_type);
            RustType::Vec(Box::new(inner))
        }
        TsTypeRef(type_ref) => resolve_type_ref(type_ref),
        TsUnionOrIntersectionType(
            swc_ecma_ast::TsUnionOrIntersectionType::TsUnionType(union),
        ) => resolve_union(union),
        TsParenthesizedType(paren) => resolve_ts_type(&paren.type_ann),
        TsOptionalType(opt) => {
            let inner = resolve_ts_type(&opt.type_ann);
            RustType::Option(Box::new(inner))
        }
        TsTupleType(tuple) => {
            let elems: Vec<_> = tuple
                .elem_types
                .iter()
                .map(|e| resolve_ts_type(&e.ty))
                .collect();
            RustType::Tuple(elems)
        }
        TsFnOrConstructorType(
            swc_ecma_ast::TsFnOrConstructorType::TsFnType(fn_type),
        ) => {
            let params: Vec<_> = fn_type
                .params
                .iter()
                .filter_map(|p| {
                    match p {
                        swc_ecma_ast::TsFnParam::Ident(ident) => {
                            ident.type_ann.as_ref().map(|t| resolve_ts_type(&t.type_ann))
                        }
                        _ => None,
                    }
                })
                .collect();
            let ret = resolve_ts_type(&fn_type.type_ann.type_ann);
            RustType::Fn {
                params,
                ret: Box::new(ret),
            }
        }
        TsLitType(lit) => resolve_literal_type(lit),
        _ => RustType::Dynamic,
    }
}

fn resolve_keyword(kw: &swc_ecma_ast::TsKeywordType) -> RustType {
    use swc_ecma_ast::TsKeywordTypeKind::*;
    match kw.kind {
        TsNumberKeyword => RustType::F64,
        TsStringKeyword => RustType::String,
        TsBooleanKeyword => RustType::Bool,
        TsVoidKeyword | TsUndefinedKeyword => RustType::Void,
        TsNullKeyword => RustType::Option(Box::new(RustType::Dynamic)),
        TsAnyKeyword | TsUnknownKeyword => RustType::Dynamic,
        TsNeverKeyword => RustType::Void,
        _ => RustType::Dynamic,
    }
}

fn resolve_type_ref(type_ref: &swc_ecma_ast::TsTypeRef) -> RustType {
    let name = match &type_ref.type_name {
        swc_ecma_ast::TsEntityName::Ident(ident) => ident.sym.to_string(),
        swc_ecma_ast::TsEntityName::TsQualifiedName(q) => {
            format!("{}.{}", qualified_name_to_string(&q.left), q.right.sym)
        }
    };

    let type_params: Vec<RustType> = type_ref
        .type_params
        .as_ref()
        .map(|params| {
            params
                .params
                .iter()
                .map(|p| resolve_ts_type(p))
                .collect()
        })
        .unwrap_or_default();

    match name.as_str() {
        "Array" => {
            let inner = type_params.into_iter().next().unwrap_or(RustType::Dynamic);
            RustType::Vec(Box::new(inner))
        }
        "Record" | "Map" => {
            let mut iter = type_params.into_iter();
            let k = iter.next().unwrap_or(RustType::String);
            let v = iter.next().unwrap_or(RustType::Dynamic);
            RustType::HashMap(Box::new(k), Box::new(v))
        }
        "Promise" => {
            type_params.into_iter().next().unwrap_or(RustType::Void)
        }
        _ => RustType::Struct(name),
    }
}

fn resolve_union(union: &swc_ecma_ast::TsUnionType) -> RustType {
    let types: Vec<_> = union.types.iter().map(|t| resolve_ts_type(t)).collect();

    // T | null | undefined → Option<T>
    let non_null: Vec<_> = types
        .iter()
        .filter(|t| !matches!(t, RustType::Void | RustType::Option(_)))
        .collect();

    let has_null = types.len() > non_null.len();

    if has_null && non_null.len() == 1 {
        return RustType::Option(Box::new(non_null[0].clone()));
    }

    if non_null.len() == 1 && !has_null {
        return non_null[0].clone();
    }

    // General union → Dynamic for now (could generate enum later)
    RustType::Dynamic
}

fn resolve_literal_type(lit: &swc_ecma_ast::TsLitType) -> RustType {
    match &lit.lit {
        swc_ecma_ast::TsLit::Number(_) => RustType::F64,
        swc_ecma_ast::TsLit::Str(_) => RustType::String,
        swc_ecma_ast::TsLit::Bool(_) => RustType::Bool,
        _ => RustType::Dynamic,
    }
}

fn qualified_name_to_string(name: &swc_ecma_ast::TsEntityName) -> String {
    match name {
        swc_ecma_ast::TsEntityName::Ident(ident) => ident.sym.to_string(),
        swc_ecma_ast::TsEntityName::TsQualifiedName(q) => {
            format!("{}.{}", qualified_name_to_string(&q.left), q.right.sym)
        }
    }
}

/// Infer a RustType from a literal expression (when no type annotation exists).
pub fn infer_from_expr(expr: &swc_ecma_ast::Expr) -> RustType {
    use swc_ecma_ast::Expr::*;
    match expr {
        Lit(lit) => infer_from_lit(lit),
        Array(arr) => {
            let elem_type = arr
                .elems
                .iter()
                .filter_map(|e| e.as_ref())
                .map(|e| infer_from_expr(&e.expr))
                .next()
                .unwrap_or(RustType::Dynamic);
            RustType::Vec(Box::new(elem_type))
        }
        Tpl(_) => RustType::String,
        Bin(bin) => {
            use swc_ecma_ast::BinaryOp::*;
            match bin.op {
                Add => {
                    let left = infer_from_expr(&bin.left);
                    if left == RustType::String {
                        RustType::String
                    } else {
                        RustType::F64
                    }
                }
                Sub | Mul | Div | Mod | Exp | BitAnd | BitOr | BitXor | LShift | RShift
                | ZeroFillRShift => RustType::F64,
                EqEq | NotEq | EqEqEq | NotEqEq | Lt | LtEq | Gt | GtEq | LogicalAnd
                | LogicalOr => RustType::Bool,
                _ => RustType::Inferred,
            }
        }
        Unary(u) => match u.op {
            swc_ecma_ast::UnaryOp::Bang => RustType::Bool,
            swc_ecma_ast::UnaryOp::Minus | swc_ecma_ast::UnaryOp::Plus => RustType::F64,
            swc_ecma_ast::UnaryOp::TypeOf => RustType::String,
            _ => RustType::Inferred,
        },
        Paren(p) => infer_from_expr(&p.expr),
        _ => RustType::Inferred,
    }
}

fn infer_from_lit(lit: &swc_ecma_ast::Lit) -> RustType {
    match lit {
        swc_ecma_ast::Lit::Num(_) => RustType::F64,
        swc_ecma_ast::Lit::Str(_) => RustType::String,
        swc_ecma_ast::Lit::Bool(_) => RustType::Bool,
        swc_ecma_ast::Lit::Null(_) => RustType::Option(Box::new(RustType::Dynamic)),
        _ => RustType::Dynamic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_type_strings() {
        assert_eq!(RustType::F64.to_rust_str(), "f64");
        assert_eq!(RustType::String.to_rust_str(), "String");
        assert_eq!(RustType::Bool.to_rust_str(), "bool");
        assert_eq!(RustType::Void.to_rust_str(), "()");
    }

    #[test]
    fn vec_type_string() {
        let t = RustType::Vec(Box::new(RustType::F64));
        assert_eq!(t.to_rust_str(), "Vec<f64>");
    }

    #[test]
    fn option_type_string() {
        let t = RustType::Option(Box::new(RustType::String));
        assert_eq!(t.to_rust_str(), "Option<String>");
    }

    #[test]
    fn hashmap_type_string() {
        let t = RustType::HashMap(Box::new(RustType::String), Box::new(RustType::F64));
        assert_eq!(t.to_rust_str(), "HashMap<String, f64>");
    }

    #[test]
    fn nested_type_string() {
        let t = RustType::Vec(Box::new(RustType::Option(Box::new(RustType::String))));
        assert_eq!(t.to_rust_str(), "Vec<Option<String>>");
    }
}
