pub mod lexer;
mod metadata;
pub mod model;
pub mod parser;
mod wml;
pub use parser::parse;

pub mod expression;

pub mod keywords;

pub fn is_wml(uri: &str) -> bool {
    uri.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("wml"))
}
