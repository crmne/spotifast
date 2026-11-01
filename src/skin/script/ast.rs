//! The syntax tree. Left-associative chains (`a+b+c`, `a.b().c`) are flat
//! vectors rather than left-deep trees, so tree depth is exactly parser
//! nesting: a chain of any length cannot recurse in the evaluator or in Drop.

use std::sync::Arc;

use super::Name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    In,
    InstanceOf,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Debug)]
pub(super) enum Link {
    Member(Name),
    Index(Expr),
    Call(Vec<Expr>),
}

#[derive(Debug)]
pub(super) enum Expr {
    Num(f64),
    Str(Name),
    Bool(bool),
    Null,
    This,
    Ident(Name),
    Array(Vec<Option<Expr>>),
    Object(Vec<(Name, Expr)>),
    Func(Arc<FuncDef>),
    Regex(Name, Name),
    /// A head followed by member accesses and calls.
    Chain(Box<Expr>, Vec<Link>),
    New(Box<Expr>, Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Update {
        prefix: bool,
        delta: f64,
        target: Box<Expr>,
    },
    /// All the operators of one precedence level, applied left to right.
    Binary(Box<Expr>, Vec<(BinOp, Expr)>),
    Cond(Box<[Expr; 3]>),
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Seq(Vec<Expr>),
}

#[derive(Debug)]
pub(super) enum Stmt {
    Empty,
    Expr(Expr),
    Var(Vec<(Name, Option<Expr>)>),
    Block(Vec<Stmt>),
    /// An `if` and all its `else if`s, flat for the same reason as chains.
    If(Vec<(Expr, Stmt)>, Option<Box<Stmt>>),
    /// `for`, `while` and `do`; `test_first` is false only for `do`.
    Loop {
        labels: Vec<Name>,
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
        test_first: bool,
    },
    ForIn {
        labels: Vec<Name>,
        target: Expr,
        object: Expr,
        body: Box<Stmt>,
    },
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Break(Option<Name>),
    Continue(Option<Name>),
    Return(Option<Expr>),
    With(Expr, Box<Stmt>),
    Try {
        block: Vec<Stmt>,
        handler: Option<(Name, Vec<Stmt>)>,
        finalizer: Option<Vec<Stmt>>,
    },
    Throw(Expr),
    Labeled(Name, Box<Stmt>),
}

/// A function body, or a whole program or handler.
#[derive(Debug)]
pub(super) struct FuncDef {
    pub name: Option<Name>,
    pub params: Vec<Name>,
    pub body: Vec<Stmt>,
    pub vars: Vec<Name>,
    pub funcs: Vec<Arc<FuncDef>>,
    pub uses_args: bool,
    /// A named function expression sees its own name.
    pub self_bind: bool,
}
