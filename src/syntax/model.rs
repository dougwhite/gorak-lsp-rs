use super::lexer::{NameId, Names, SyntaxError, Token};
use crate::source::{Source, Span};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Class,
    Method,
    Function,
    Variable,
    Field,
    Property,
    Constant,
    Frame,
}
impl SymbolKind {
    pub fn lsp(self) -> u32 {
        match self {
            Self::Class => 5,
            Self::Method => 6,
            Self::Function => 12,
            Self::Variable => 13,
            Self::Field => 8,
            Self::Property => 7,
            Self::Constant => 14,
            Self::Frame => 19,
        }
    }
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TypeRef {
    pub name: NameId,
    pub display: String,
    pub array: bool,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Symbol {
    pub name: NameId,
    pub spelling: String,
    pub span: Span,
    pub scope: u32,
    pub kind: SymbolKind,
    pub ty: Option<TypeRef>,
    pub constant_value: Option<String>,
    pub implementation: bool,
    pub local: bool,
    pub parameter: bool,
    pub owner: Option<SymbolId>,
    pub parameters: Vec<SymbolId>,
    pub field_scope: Option<u32>,
    /// Declaration and implementation share a canonical identity.
    pub canonical: SymbolId,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Component,
    ClassLocals,
    Routine,
    Field,
    Property,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Scope {
    pub parent: Option<u32>,
    pub kind: ScopeKind,
    pub owner: Option<SymbolId>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub name: String,
    pub span: Span,
    pub selection: Span,
    pub scope: u32,
    pub kind: u32,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Document {
    pub facts: Facts,
    pub source: Source,
    pub component: String,
    pub component_kind: String,
    pub superclass: Option<String>,
    pub names: Names,
    pub tokens: Vec<Token>,
    pub symbols: Vec<Symbol>,
    pub scopes: Vec<Scope>,
    pub blocks: Vec<Block>,
    pub errors: Vec<SyntaxError>,
}
impl Document {
    pub fn new(source: Source) -> Self {
        let component = crate::source::uri_path(&source.uri)
            .and_then(|path| {
                path.file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| {
                source
                    .uri
                    .rsplit('/')
                    .next()
                    .unwrap_or("source")
                    .rsplit_once('.')
                    .map_or("source", |(name, _)| name)
                    .to_owned()
            });
        Self {
            facts: Facts::default(),
            source,
            component,
            component_kind: String::new(),
            superclass: None,
            names: Names::default(),
            tokens: Vec::new(),
            symbols: Vec::new(),
            scopes: vec![Scope {
                parent: None,
                kind: ScopeKind::Component,
                owner: None,
            }],
            blocks: Vec::new(),
            errors: Vec::new(),
        }
    }
    pub fn scope(&mut self, parent: u32, kind: ScopeKind) -> u32 {
        let id = self.scopes.len() as u32;
        self.scopes.push(Scope {
            parent: Some(parent),
            kind,
            owner: None,
        });
        id
    }
    pub fn declare(
        &mut self,
        spelling: &str,
        span: Span,
        scope: u32,
        kind: SymbolKind,
    ) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        let name = self.names.intern(spelling);
        self.symbols.push(Symbol {
            name,
            spelling: spelling.into(),
            span,
            scope,
            kind,
            ty: None,
            constant_value: None,
            implementation: false,
            local: false,
            parameter: false,
            owner: None,
            parameters: Vec::new(),
            field_scope: None,
            canonical: id,
        });
        id
    }
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }
    pub fn symbol_mut(&mut self, id: SymbolId) -> &mut Symbol {
        &mut self.symbols[id.0 as usize]
    }
    pub fn token_at(&self, byte: u32) -> Option<usize> {
        let at = self.tokens.partition_point(|t| t.span.start <= byte);
        at.checked_sub(1).filter(|&i| {
            self.tokens[i].span.end >= byte && self.tokens[i].kind == super::lexer::Kind::Name
        })
    }
    pub fn canonical(&self, id: SymbolId) -> SymbolId {
        self.symbol(id).canonical
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Facts {
    pub requires_preprocessing: bool,
    pub dynamic_calls: bool,
    pub dynamic_dispatch: bool,
    pub literal_names: std::collections::HashSet<String>,
}
impl Facts {
    pub fn gather(document: &Document) -> Self {
        use super::lexer::Kind;
        let requires_preprocessing = !super::is_wml(&document.source.uri)
            && document.source.text()[crate::preprocessor::body_start(document.source.text())..]
                .lines()
                .any(|line| line.starts_with('#'));
        let dynamic_dispatch = document
            .tokens
            .windows(2)
            .any(|pair| pair[0].kind == Kind::Punct(b'.') && pair[1].kind == Kind::Punct(b':'));
        let dynamic_calls = dynamic_dispatch
            || document.tokens.iter().enumerate().any(|(i, t)| {
                t.kind == Kind::Name
                    && (document.names.get(t.name) == "execute"
                        || ["callproc", "callframe", "openframe", "gotoframe"]
                            .contains(&document.names.get(t.name))
                            && document
                                .tokens
                                .get(i + 1)
                                .is_some_and(|t| t.kind == Kind::Punct(b':')))
            });
        Self {
            requires_preprocessing,
            dynamic_calls,
            dynamic_dispatch,
            literal_names: document.facts.literal_names.clone(),
        }
    }
}
