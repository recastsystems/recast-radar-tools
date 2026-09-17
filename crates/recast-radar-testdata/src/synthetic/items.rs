//! Item structure of a lexed Rust file: which functions, constants, types and
//! macros are test code, and where they start and end.

use super::lexer::{Lexed, Tok};

/// Kind of a test-code item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    Fn,
    Const,
    Static,
    Struct,
    Enum,
    Type,
    Macro,
}

/// One item inside test code. Nested functions and closures belong to the
/// enclosing item.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    /// Inline module path from the file root (e.g. `["tests"]`).
    pub(crate) mod_path: Vec<String>,
    /// Self type when the item is a method in an `impl` or `trait` block.
    pub(crate) impl_type: Option<String>,
    /// Item name (`_` for anonymous constants).
    pub(crate) name: String,
    pub(crate) kind: ItemKind,
    /// Token range: first token after the attributes, and exclusive end.
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// Index of the name token.
    pub(crate) name_token: usize,
    /// Carries `#[test]` (or another `*test` attribute).
    pub(crate) is_test_fn: bool,
}

impl Item {
    /// `mod::path::Type::name`, the key used by the allowlist.
    pub(crate) fn qualified_name(&self) -> String {
        let mut parts: Vec<&str> = self.mod_path.iter().map(String::as_str).collect();
        if let Some(impl_type) = &self.impl_type {
            parts.push(impl_type);
        }
        parts.push(&self.name);
        parts.join("::")
    }
}

/// An out-of-line module declared inside test code (`#[cfg(test)] mod x;`).
#[derive(Clone, Debug)]
pub(crate) struct ExternalTestModule {
    /// Inline module path of the declaration, including the module's name.
    pub(crate) mod_path: Vec<String>,
}

#[derive(Clone)]
struct Scope {
    mod_path: Vec<String>,
    impl_type: Option<String>,
    test: bool,
}

/// Test-code items of a file. `whole_file_is_test` is true for files under a
/// crate's `tests/` directory (and for modules declared from test code).
pub(crate) fn test_items(
    lexed: &Lexed,
    whole_file_is_test: bool,
    mod_prefix: &[String],
) -> (Vec<Item>, Vec<ExternalTestModule>) {
    let mut items = Vec::new();
    let mut modules = Vec::new();
    let scope = Scope {
        mod_path: mod_prefix.to_vec(),
        impl_type: None,
        test: whole_file_is_test,
    };
    parse_block(lexed, 0, lexed.len(), scope, &mut items, &mut modules);
    (items, modules)
}

#[derive(Default)]
struct Attrs {
    cfg_test: bool,
    test_fn: bool,
}

fn analyze_attr(lexed: &Lexed, open: usize, close: usize, attrs: &mut Attrs) {
    let idents: Vec<&str> = (open + 1..close).filter_map(|i| lexed.ident(i)).collect();
    match idents.first().copied() {
        Some("cfg") => {
            if idents.contains(&"test") && !idents.contains(&"not") {
                attrs.cfg_test = true;
            }
        }
        Some(_) => {
            // #[test], #[tokio::test], #[rstest], ...: a path whose last
            // segment ends in `test`, with no arguments.
            let has_args = (open + 1..close).any(|i| matches!(lexed.tok(i), Some(Tok::Open(_))));
            if !has_args && idents.last().is_some_and(|last| last.ends_with("test")) {
                attrs.test_fn = true;
            }
        }
        None => {}
    }
}

fn parse_block(
    lexed: &Lexed,
    start: usize,
    end: usize,
    mut scope: Scope,
    items: &mut Vec<Item>,
    modules: &mut Vec<ExternalTestModule>,
) {
    let mut i = start;
    while i < end {
        let before = i;
        let mut attrs = Attrs::default();
        while lexed.is_punct(i, "#") {
            let inner = lexed.is_punct(i + 1, "!");
            let open = if inner { i + 2 } else { i + 1 };
            if !lexed.is_open(open, '[') {
                break;
            }
            let Some(close) = lexed.close_of(open) else {
                break;
            };
            let mut attr = Attrs::default();
            analyze_attr(lexed, open, close, &mut attr);
            if inner {
                scope.test |= attr.cfg_test;
            } else {
                attrs.cfg_test |= attr.cfg_test;
                attrs.test_fn |= attr.test_fn;
            }
            i = close + 1;
        }
        if i >= end {
            break;
        }
        // Visibility and qualifiers.
        loop {
            match lexed.ident(i) {
                Some("pub") => {
                    i += 1;
                    if lexed.is_open(i, '(') {
                        i = lexed.close_of(i).map_or(i + 1, |close| close + 1);
                    }
                }
                Some("unsafe" | "async" | "default") => i += 1,
                Some("extern") if matches!(lexed.tok(i + 1), Some(Tok::Str(_))) => {
                    if lexed.is_ident(i + 2, "fn") {
                        i += 2;
                    } else {
                        break;
                    }
                }
                Some("const") if matches!(lexed.ident(i + 1), Some("fn" | "unsafe" | "async")) => {
                    i += 1
                }
                _ => break,
            }
        }
        let is_test = scope.test || attrs.cfg_test || attrs.test_fn;
        let item_start = i;
        let keyword = lexed.ident(i).map(str::to_owned);
        match keyword.as_deref() {
            Some("fn") => {
                let body_end = fn_end(lexed, i + 2, end);
                if is_test && let Some(name) = lexed.ident(i + 1) {
                    items.push(Item {
                        mod_path: scope.mod_path.clone(),
                        impl_type: scope.impl_type.clone(),
                        name: name.to_owned(),
                        kind: ItemKind::Fn,
                        start: item_start,
                        end: body_end,
                        name_token: i + 1,
                        is_test_fn: attrs.test_fn,
                    });
                }
                i = body_end;
            }
            Some("mod") => {
                let name = lexed.ident(i + 1).unwrap_or("_").to_owned();
                let mut mod_path = scope.mod_path.clone();
                mod_path.push(name);
                if lexed.is_open(i + 2, '{') {
                    let close = lexed.close_of(i + 2).unwrap_or(end);
                    let inner = Scope {
                        mod_path,
                        impl_type: None,
                        test: is_test,
                    };
                    parse_block(lexed, i + 3, close, inner, items, modules);
                    i = close + 1;
                } else {
                    if is_test {
                        modules.push(ExternalTestModule { mod_path });
                    }
                    i = semicolon_end(lexed, i + 1, end);
                }
            }
            Some("impl" | "trait") => {
                let Some(brace) = first_brace(lexed, i + 1, end) else {
                    i = semicolon_end(lexed, i + 1, end);
                    continue;
                };
                let self_type = if keyword.as_deref() == Some("trait") {
                    lexed.ident(i + 1).map(str::to_owned)
                } else {
                    impl_self_type(lexed, i + 1, brace)
                };
                let close = lexed.close_of(brace).unwrap_or(end);
                let inner = Scope {
                    mod_path: scope.mod_path.clone(),
                    impl_type: self_type,
                    test: is_test,
                };
                parse_block(lexed, brace + 1, close, inner, items, modules);
                i = close + 1;
            }
            Some(kw @ ("struct" | "enum" | "union" | "const" | "static" | "type")) => {
                let (kind, name_token) = match kw {
                    "struct" | "union" => (ItemKind::Struct, i + 1),
                    "enum" => (ItemKind::Enum, i + 1),
                    "type" => (ItemKind::Type, i + 1),
                    "static" if lexed.is_ident(i + 1, "mut") => (ItemKind::Static, i + 2),
                    "static" => (ItemKind::Static, i + 1),
                    _ => (ItemKind::Const, i + 1),
                };
                let item_end = if matches!(kind, ItemKind::Struct | ItemKind::Enum) {
                    struct_end(lexed, name_token + 1, end)
                } else {
                    semicolon_end(lexed, name_token + 1, end)
                };
                if is_test && let Some(name) = lexed.ident(name_token) {
                    items.push(Item {
                        mod_path: scope.mod_path.clone(),
                        impl_type: scope.impl_type.clone(),
                        name: name.to_owned(),
                        kind,
                        start: item_start,
                        end: item_end,
                        name_token,
                        is_test_fn: false,
                    });
                }
                i = item_end;
            }
            Some("use" | "extern") => i = semicolon_end(lexed, i + 1, end),
            Some("macro_rules") if lexed.is_punct(i + 1, "!") => {
                let open = i + 3;
                let close = lexed.close_of(open).unwrap_or(end);
                if is_test && let Some(name) = lexed.ident(i + 2) {
                    items.push(Item {
                        mod_path: scope.mod_path.clone(),
                        impl_type: scope.impl_type.clone(),
                        name: name.to_owned(),
                        kind: ItemKind::Macro,
                        start: item_start,
                        end: close + 1,
                        name_token: i + 2,
                        is_test_fn: false,
                    });
                }
                i = close + 1;
            }
            Some(_) if lexed.is_punct(i + 1, "!") => {
                // Item-position macro invocation (thread_local! {...}).
                let open = i + 2;
                i = lexed.close_of(open).map_or(i + 2, |close| close + 1);
            }
            _ => {
                i = match lexed.tok(i) {
                    Some(Tok::Open(_)) => lexed.close_of(i).map_or(i + 1, |close| close + 1),
                    _ => i + 1,
                };
            }
        }
        if i <= before {
            i = before + 1;
        }
    }
}

/// End (exclusive) of a function starting at its signature: the body's
/// closing brace, or the `;` of a declaration.
fn fn_end(lexed: &Lexed, mut j: usize, end: usize) -> usize {
    while j < end {
        match lexed.tok(j) {
            Some(Tok::Open('{')) => return lexed.close_of(j).map_or(end, |close| close + 1),
            Some(Tok::Open(_)) => j = lexed.close_of(j).map_or(j + 1, |close| close + 1),
            Some(Tok::Punct(";")) => return j + 1,
            _ => j += 1,
        }
    }
    end
}

fn struct_end(lexed: &Lexed, mut j: usize, end: usize) -> usize {
    while j < end {
        match lexed.tok(j) {
            Some(Tok::Open('{')) => return lexed.close_of(j).map_or(end, |close| close + 1),
            Some(Tok::Open(_)) => j = lexed.close_of(j).map_or(j + 1, |close| close + 1),
            Some(Tok::Punct(";")) => return j + 1,
            _ => j += 1,
        }
    }
    end
}

fn semicolon_end(lexed: &Lexed, mut j: usize, end: usize) -> usize {
    while j < end {
        match lexed.tok(j) {
            Some(Tok::Open(_)) => j = lexed.close_of(j).map_or(j + 1, |close| close + 1),
            Some(Tok::Punct(";")) => return j + 1,
            _ => j += 1,
        }
    }
    end
}

fn first_brace(lexed: &Lexed, mut j: usize, end: usize) -> Option<usize> {
    while j < end {
        match lexed.tok(j) {
            Some(Tok::Open('{')) => return Some(j),
            Some(Tok::Open(_)) => j = lexed.close_of(j).map_or(j + 1, |close| close + 1),
            Some(Tok::Punct(";")) => return None,
            _ => j += 1,
        }
    }
    None
}

/// Self type of `impl<..> Trait for Type<..> where .. {`: the last path
/// segment before generics, `where` or the brace.
fn impl_self_type(lexed: &Lexed, start: usize, brace: usize) -> Option<String> {
    let mut j = start;
    // Skip `<...>` generic parameters right after `impl`.
    if lexed.is_punct(j, "<") {
        let mut depth = 0i32;
        while j < brace {
            if lexed.is_punct(j, "<") {
                depth += 1;
            } else if lexed.is_punct(j, ">") {
                depth -= 1;
                if depth == 0 {
                    j += 1;
                    break;
                }
            }
            j += 1;
        }
    }
    let type_start = (j..brace)
        .find(|&k| lexed.is_ident(k, "for") && !lexed.is_punct(k + 1, "<"))
        .map_or(j, |k| k + 1);
    let mut name = None;
    let mut k = type_start;
    while k < brace {
        if lexed.is_punct(k, "<") || lexed.is_ident(k, "where") {
            break;
        }
        if let Some(ident) = lexed.ident(k)
            && !matches!(ident, "dyn" | "mut" | "impl" | "crate" | "super" | "self")
        {
            name = Some(ident.to_owned());
        }
        k += 1;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str, whole: bool) -> Vec<String> {
        let lexed = Lexed::new(source);
        test_items(&lexed, whole, &[])
            .0
            .iter()
            .map(Item::qualified_name)
            .collect()
    }

    #[test]
    fn only_cfg_test_and_test_fns_are_items_in_src_files() {
        let source = r#"
            pub fn library() {}
            #[cfg(test)]
            mod tests {
                use super::*;
                const K: u8 = 1;
                struct Helper;
                impl Helper { fn build(&self) {} }
                fn helper() -> Vec<u8> { vec![] }
                #[test]
                fn case() { helper(); }
            }
            impl Library {
                #[cfg(test)]
                fn only_in_tests(&self) {}
                fn not_test(&self) {}
            }
            #[cfg(not(test))]
            fn production() {}
            #[cfg(test)]
            const _: () = { assert!(true); };
        "#;
        assert_eq!(
            names(source, false),
            vec![
                "tests::K",
                "tests::Helper",
                "tests::Helper::build",
                "tests::helper",
                "tests::case",
                "Library::only_in_tests",
                "_",
            ]
        );
    }

    #[test]
    fn tests_directory_files_are_test_code_throughout() {
        let source = "const DATA: &[u8] = &[]; fn helper() {} #[test] fn t() { fn inner() {} }";
        assert_eq!(names(source, true), vec!["DATA", "helper", "t"]);
        let lexed = Lexed::new(source);
        let (items, _) = test_items(&lexed, true, &[]);
        assert!(items.iter().any(|item| item.name == "t" && item.is_test_fn));
    }

    #[test]
    fn impl_self_type_takes_the_implemented_type() {
        let source = "#[cfg(test)] mod m { impl<'a, T: Clone> From<Vec<T>> for crate::Wrap<'a, T> where T: Copy { fn from(v: Vec<T>) -> Self { todo!() } } }";
        assert_eq!(names(source, false), vec!["m::Wrap::from"]);
    }

    #[test]
    fn external_test_modules_are_reported() {
        let lexed = Lexed::new("#[cfg(test)] mod tests; mod other;");
        let (_, modules) = test_items(&lexed, false, &[]);
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].mod_path, vec!["tests".to_owned()]);
    }
}
