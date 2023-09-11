use alloc::boxed::Box;
use alloc::vec::Vec;
use chumsky::{error::Error, prelude::*};
use core::fmt;
use tinyvec_string::TinyString;

//use crate::scale::PrimitiveType;

pub type String = TinyString<[u8; 24]>;

type Span = SimpleSpan<usize>;

#[derive(Clone, Debug, PartialEq)]
enum Token<'src> {
    Num(u32),
    Op(char),
    Ident(&'src str),
}

impl<'src> fmt::Display for Token<'src> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Token::Num(n) => write!(f, "{}", n),
            Token::Op(c) => write!(f, "{}", c),
            Token::Ident(s) => write!(f, "{}", s),
        }
    }
}

fn lexer<'src>(
) -> impl Parser<'src, &'src str, Vec<(Token<'src>, Span)>, extra::Err<Simple<'src, char, Span>>> {
    // A parser for numbers
    let num = text::int(10)
        .try_map(|s: &str, span| {
            s.parse::<u32>().or(Err(Error::<&str>::expected_found(
                [],
                s.chars().next().map(Into::into),
                span,
            )))
        })
        .map(Token::Num);
    // A parser for control characters (delimiters, semicolons, etc.)
    let op = one_of("|=@:;,#()[]{}<>").map(Token::Op);
    // A parser for identifiers and keywords
    let ident = text::ident().map(Token::Ident);
    // A single token can be one of the above
    let token = num.or(op).or(ident);
    let comment = just("//")
        .then(any().and_is(just('\n').not()).repeated())
        .padded();
    token
        .map_with_span(|tok, span| (tok, span))
        .padded_by(comment.repeated())
        .padded()
        .repeated()
        .collect()
}

#[derive(Debug, Clone)]
pub enum Id {
    Name(String),
    Num(u32),
    Type(Box<Type>),
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Self::Name(s.into())
    }
}

impl From<String> for Id {
    fn from(s: String) -> Self {
        Self::Name(s)
    }
}

impl From<u32> for Id {
    fn from(n: u32) -> Self {
        Self::Num(n)
    }
}

#[derive(Debug, Clone)]
pub struct Enum {
    pub variants: Vec<(String, Option<Id>, Option<u32>)>,
}

impl Enum {
    pub fn new(variants: Vec<(String, Option<Id>, Option<u32>)>) -> Self {
        Self { variants }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimitiveType {
    U8,
    U16,
    U32,
    U64,
    U128,
    I8,
    I16,
    I32,
    I64,
    I128,
    Bool,
    Str,
}

impl core::str::FromStr for PrimitiveType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str(s).cloned().ok_or(())
    }
}

#[derive(Debug, Clone)]
pub enum Type {
    Primitive(PrimitiveType),
    Compact(Id),
    Seq(Id),
    Tuple(Vec<Id>),
    Array(Id, u32),
    Enum(Enum),
    Struct(Vec<(String, Id)>),
    Alias(Id),
}

macro_rules! impl_primitive_types {
    ($(($id:literal, $ty:ident)),*) => {
        impl Type {
            pub fn primitive(s: &str) -> Option<&'static Self> {
                match s {
                    $(
                        $id => Some(&Self::Primitive(PrimitiveType::$ty)),
                    )*
                    _ => None,
                }
            }
        }
        fn primitive_parser<'tokens, 'src: 'tokens, E>(
        ) -> impl Parser<'tokens, ParserInput<'tokens, 'src>, PrimitiveType, E> + Clone
        where
            E: extra::ParserExtra<'tokens, ParserInput<'tokens, 'src>>,
        {
            use Token::*;
            choice((
                $(just(Ident($id)).map(|_| PrimitiveType::$ty),)*
            ))
        }
        impl PrimitiveType {
            pub fn from_str(s: &str) -> Option<&'static PrimitiveType> {
                let ty = match s {
                    $(
                        $id => &PrimitiveType::$ty,
                    )*
                    _ => return None,
                };
                Some(ty)
            }
        }
    };
}

impl_primitive_types! {
    ("u8", U8),
    ("u16", U16),
    ("u32", U32),
    ("u64", U64),
    ("u128", U128),
    ("i8", I8),
    ("i16", I16),
    ("i32", I32),
    ("i64", I64),
    ("i128", I128),
    ("bool", Bool),
    ("str", Str)
}

impl Type {
    pub fn is_alias(&self) -> bool {
        matches!(self, Self::Alias(_))
    }
}

impl From<PrimitiveType> for Type {
    fn from(ty: PrimitiveType) -> Self {
        Self::Primitive(ty)
    }
}

#[derive(Clone, Debug)]
pub struct TypeDef {
    pub name: Option<String>,
    pub ty: Type,
}

type ParserInput<'tokens, 'src> =
    chumsky::input::SpannedInput<Token<'src>, Span, &'tokens [(Token<'src>, Span)]>;

fn type_parser<'tokens, 'src: 'tokens, E>(
) -> impl Parser<'tokens, ParserInput<'tokens, 'src>, Type, E> + Clone
where
    E: extra::ParserExtra<'tokens, ParserInput<'tokens, 'src>>,
{
    recursive(|typedef| {
        use Token::*;
        let ident = select! { Ident(ident) => String::from(ident) };
        let tid = select! {
            Ident(ident) => Id::Name(ident.into()),
            Num(n) => Id::Num(n),
        };
        let typ = tid.or(typedef.map(|t| Id::Type(Box::new(t))));
        let num = select! { Num(v) => v };
        // A list of type identifiers
        let tids = typ
            .clone()
            .separated_by(just(Op(',')))
            .allow_trailing()
            .collect::<Vec<_>>();
        let compact_def = just(Op('@')).ignore_then(typ.clone()).map(Type::Compact);
        let tuple_def = just(Op('('))
            .ignore_then(tids)
            .then_ignore(just(Op(')')))
            .map(Type::Tuple);
        let array_def = just(Op('['))
            .ignore_then(typ.clone().then_ignore(just(Op(';'))).then(num))
            .then_ignore(just(Op(']')))
            .map(|(ty, len)| Type::Array(ty, len));
        let seq_def = just(Op('['))
            .ignore_then(typ.clone())
            .then_ignore(just(Op(']')))
            .map(Type::Seq);
        let enum_variant = ident
            .then(just(Op(':')).ignore_then(typ.clone().or_not()).or_not())
            .then(just(Op(':')).ignore_then(num).or_not())
            .map(|((name, t), i)| (name, t.flatten(), i));
        let enum_def = just(Op('<'))
            .ignore_then(
                enum_variant
                    .separated_by(just(Op(',')).or(just(Op('|'))))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .map(|vec| Type::Enum(Enum::new(vec)))
            .then_ignore(just(Op('>')));
        let struct_field = ident
            .then(just(Op(':')).ignore_then(typ.clone()))
            .map(|(name, tid)| (name, tid));
        let struct_def = just(Op('{'))
            .ignore_then(
                struct_field
                    .separated_by(just(Op(',')))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Op('}')))
            .map(Type::Struct);
        let alias_def = tid.map(Type::Alias);
        let primitive_def = just(Op('#'))
            .ignore_then(primitive_parser())
            .map(Type::Primitive);
        choice((
            primitive_def,
            alias_def,
            compact_def,
            seq_def,
            array_def,
            tuple_def,
            enum_def,
            struct_def,
        ))
    })
}

fn parser<'tokens, 'src: 'tokens>() -> impl Parser<
    'tokens,
    ParserInput<'tokens, 'src>,
    Vec<TypeDef>,
    extra::Err<Rich<'tokens, Token<'src>, Span>>,
> + Clone {
    use Token::*;
    let ty = type_parser();
    let ident = select! { Ident(ident) => String::from(ident) };
    let stmt = ident
        .then_ignore(just(Op('=')))
        .or_not()
        .then(ty)
        .map(|(name, ty)| TypeDef { name, ty });
    stmt.separated_by(just(Op(';')).or_not())
        .allow_trailing()
        .collect::<Vec<_>>()
        .then_ignore(end())
}

pub fn parse_types(src: &str) -> js::Result<Vec<TypeDef>> {
    let tokens = lexer()
        .parse(src)
        .into_result()
        .map_err(super::to_js_error)?;
    let ast = parser()
        .parse(tokens.as_slice().spanned((src.len()..src.len()).into()))
        .into_result()
        .map_err(super::to_js_error)?;
    Ok(ast)
}

#[test]
fn it_works() {
    let src = "foo=[u8;32];bar=(u8,foo)";
    let tokens = lexer().parse(src).unwrap();
    let ast = parser()
        .parse(tokens.as_slice().spanned((src.len()..src.len()).into()))
        .into_result();
    println!("{:#?}", ast);
    assert!(ast.is_ok());
}
