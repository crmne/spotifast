//! Recursive descent over [`Lexer`]. Every recursive path passes
//! [`Parser::nest`], so hostile nesting ends in [`ScriptError::Depth`], not a
//! stack overflow.

use std::collections::HashSet;
use std::sync::Arc;

use super::ast::{BinOp, Expr, FuncDef, Link, Stmt, UnOp};
use super::convert::number_to_string;
use super::lexer::{Interner, Lexer, Tok};
use super::{Name, ScriptError};

type P<T> = Result<T, ScriptError>;

/// Syntactic nesting, in statements, assignment expressions and operator
/// levels. Real skins stay well under this. It also bounds the tree, whose
/// drop is recursive. The stack itself is what `stack` measures: one level is
/// about 10 KiB in an unoptimised build, nine frames per parenthesis.
pub(super) const MAX_NESTING: u32 = 64;

#[derive(Default)]
struct FnCtx {
    vars: Vec<Name>,
    seen: HashSet<Name>,
    funcs: Vec<Arc<FuncDef>>,
    uses_args: bool,
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    tok: Tok,
    newline: bool,
    /// Where the last `/` token began, for rescanning it as a regex.
    slash: usize,
    depth: u32,
    limit: u32,
    no_in: bool,
    labels: Vec<Name>,
    ctx: Vec<FnCtx>,
}

/// Parses a whole program, handler or `eval` string, to `limit` levels deep.
pub(super) fn parse(source: &str, names: &mut Interner, limit: u32) -> P<Arc<FuncDef>> {
    let mut parser = Parser {
        lexer: Lexer::new(source, names),
        tok: Tok::Eof,
        newline: false,
        slash: 0,
        depth: 0,
        limit,
        no_in: false,
        labels: Vec::new(),
        ctx: vec![FnCtx::default()],
    };
    parser.advance()?;
    let mut body = Vec::new();
    while parser.tok != Tok::Eof {
        body.push(parser.statement()?);
    }
    let ctx = parser.ctx.pop().unwrap_or_default();
    Ok(Arc::new(FuncDef {
        name: None,
        params: Vec::new(),
        body,
        vars: ctx.vars,
        funcs: ctx.funcs,
        uses_args: false,
        self_bind: false,
    }))
}

fn is_place(e: &Expr) -> bool {
    match e {
        Expr::Ident(_) => true,
        Expr::Chain(_, links) => !matches!(links.last(), Some(Link::Call(_)) | None),
        _ => false,
    }
}

impl Parser<'_> {
    fn advance(&mut self) -> P<()> {
        let (tok, newline) = self.lexer.next()?;
        if let Tok::P(p @ ("/" | "/=")) = tok {
            self.slash = self.lexer.pos - p.len();
        }
        self.tok = tok;
        self.newline = newline;
        Ok(())
    }

    fn fail<T>(&self, message: &str) -> P<T> {
        Err(ScriptError::Syntax {
            line: self.lexer.line,
            message: message.to_string(),
        })
    }

    fn nest(&mut self) -> P<()> {
        self.depth += 1;
        if self.depth > self.limit || super::stack::exceeded() {
            return Err(ScriptError::Depth);
        }
        Ok(())
    }

    fn is(&self, p: &str) -> bool {
        matches!(&self.tok, Tok::P(t) | Tok::Kw(t) if *t == p)
    }

    fn eat(&mut self, p: &str) -> P<bool> {
        let found = self.is(p);
        if found {
            self.advance()?;
        }
        Ok(found)
    }

    fn expect(&mut self, p: &str) -> P<()> {
        if self.eat(p)? {
            return Ok(());
        }
        self.fail(&format!("expected '{p}'"))
    }

    fn ident(&mut self) -> P<Name> {
        if let Tok::Ident(name) = &self.tok {
            let name = name.clone();
            self.advance()?;
            return Ok(name);
        }
        self.fail("expected an identifier")
    }

    /// Automatic semicolon insertion.
    fn semicolon(&mut self) -> P<()> {
        if self.eat(";")? || self.is("}") || self.tok == Tok::Eof || self.newline {
            return Ok(());
        }
        self.fail("expected ';'")
    }

    fn declare(&mut self, name: &Name) {
        if let Some(ctx) = self.ctx.last_mut()
            && ctx.seen.insert(name.clone())
        {
            ctx.vars.push(name.clone());
        }
    }

    fn statement(&mut self) -> P<Stmt> {
        self.nest()?;
        let result = self.statement_inner();
        self.depth -= 1;
        result
    }

    fn block(&mut self) -> P<Vec<Stmt>> {
        self.expect("{")?;
        let mut body = Vec::new();
        while !self.is("}") {
            if self.tok == Tok::Eof {
                return self.fail("expected '}'");
            }
            body.push(self.statement()?);
        }
        self.advance()?;
        Ok(body)
    }

    fn statement_inner(&mut self) -> P<Stmt> {
        // A label applies only to the statement straight after it.
        let labels = std::mem::take(&mut self.labels);
        let keyword = match &self.tok {
            Tok::P("{") => return Ok(Stmt::Block(self.block()?)),
            Tok::P(";") => {
                self.advance()?;
                return Ok(Stmt::Empty);
            }
            Tok::Kw(k) => *k,
            _ => "",
        };
        match keyword {
            "var" => {
                self.advance()?;
                let list = self.var_list()?;
                self.semicolon()?;
                Ok(Stmt::Var(list))
            }
            "function" => self.function_declaration(),
            "if" => self.if_statement(),
            "while" | "do" => self.while_statement(keyword == "while", labels),
            "for" => self.for_statement(labels),
            "switch" => self.switch_statement(),
            "break" | "continue" => {
                self.advance()?;
                let label = match &self.tok {
                    Tok::Ident(_) if !self.newline => Some(self.ident()?),
                    _ => None,
                };
                self.semicolon()?;
                Ok(if keyword == "break" {
                    Stmt::Break(label)
                } else {
                    Stmt::Continue(label)
                })
            }
            "return" => {
                self.advance()?;
                let ends = self.newline || self.is(";") || self.is("}") || self.tok == Tok::Eof;
                let value = if ends { None } else { Some(self.expression()?) };
                self.semicolon()?;
                Ok(Stmt::Return(value))
            }
            "throw" => {
                self.advance()?;
                if self.newline {
                    return self.fail("no line break is allowed after throw");
                }
                let value = self.expression()?;
                self.semicolon()?;
                Ok(Stmt::Throw(value))
            }
            "with" => {
                self.advance()?;
                let object = self.paren_expression()?;
                Ok(Stmt::With(object, Box::new(self.statement()?)))
            }
            "try" => self.try_statement(),
            _ => {
                let expression = self.expression()?;
                if let (Expr::Ident(name), true) = (&expression, self.is(":")) {
                    let name = name.clone();
                    self.advance()?;
                    self.labels = labels;
                    self.labels.push(name.clone());
                    return Ok(Stmt::Labeled(name, Box::new(self.statement()?)));
                }
                self.semicolon()?;
                Ok(Stmt::Expr(expression))
            }
        }
    }

    fn function_declaration(&mut self) -> P<Stmt> {
        self.advance()?;
        let name = self.ident()?;
        // JScript's `function object::event()` wires an event, which a
        // sandbox cannot do; parse it so the rest of the file still loads.
        if self.eat(":")? {
            self.expect(":")?;
            self.ident()?;
        }
        let def = self.function(Some(name), false)?;
        if let Some(ctx) = self.ctx.last_mut() {
            ctx.funcs.push(def);
        }
        Ok(Stmt::Empty)
    }

    fn var_list(&mut self) -> P<Vec<(Name, Option<Expr>)>> {
        let mut list = Vec::new();
        loop {
            let name = self.ident()?;
            self.declare(&name);
            let init = if self.eat("=")? {
                Some(self.assign()?)
            } else {
                None
            };
            list.push((name, init));
            if !self.eat(",")? {
                return Ok(list);
            }
        }
    }

    fn paren_expression(&mut self) -> P<Expr> {
        self.expect("(")?;
        let value = self.expression()?;
        self.expect(")")?;
        Ok(value)
    }

    fn if_statement(&mut self) -> P<Stmt> {
        let mut arms = Vec::new();
        let mut otherwise = None;
        loop {
            self.advance()?;
            let cond = self.paren_expression()?;
            arms.push((cond, self.statement()?));
            if !self.eat("else")? {
                break;
            }
            if !self.is("if") {
                otherwise = Some(Box::new(self.statement()?));
                break;
            }
        }
        Ok(Stmt::If(arms, otherwise))
    }

    fn while_statement(&mut self, test_first: bool, labels: Vec<Name>) -> P<Stmt> {
        self.advance()?;
        let (cond, body);
        if test_first {
            cond = self.paren_expression()?;
            body = self.statement()?;
        } else {
            body = self.statement()?;
            self.expect("while")?;
            cond = self.paren_expression()?;
            self.eat(";")?;
        }
        Ok(Stmt::Loop {
            labels,
            init: None,
            cond: Some(cond),
            update: None,
            body: Box::new(body),
            test_first,
        })
    }

    fn for_statement(&mut self, labels: Vec<Name>) -> P<Stmt> {
        self.advance()?;
        self.expect("(")?;
        self.no_in = true;
        let mut target = None;
        let init = if self.is(";") {
            None
        } else if self.eat("var")? {
            let list = self.var_list()?;
            match list.as_slice() {
                [(name, None)] if self.is("in") => {
                    target = Some(Expr::Ident(name.clone()));
                    None
                }
                _ => Some(Stmt::Var(list)),
            }
        } else {
            let e = self.expression()?;
            if self.is("in") {
                target = Some(e);
                None
            } else {
                Some(Stmt::Expr(e))
            }
        };
        self.no_in = false;
        if let Some(target) = target {
            if !is_place(&target) {
                return self.fail("bad for-in target");
            }
            self.advance()?;
            let object = self.expression()?;
            self.expect(")")?;
            let body = Box::new(self.statement()?);
            return Ok(Stmt::ForIn {
                labels,
                target,
                object,
                body,
            });
        }
        self.expect(";")?;
        let cond = if self.is(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(";")?;
        let update = if self.is(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(")")?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::Loop {
            labels,
            init: init.map(Box::new),
            cond,
            update,
            body,
            test_first: true,
        })
    }

    fn switch_statement(&mut self) -> P<Stmt> {
        self.advance()?;
        let subject = self.paren_expression()?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.eat("}")? {
            let test = if self.eat("default")? {
                None
            } else {
                self.expect("case")?;
                Some(self.expression()?)
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !(self.is("case") || self.is("default") || self.is("}")) {
                if self.tok == Tok::Eof {
                    return self.fail("expected '}'");
                }
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(subject, cases))
    }

    fn try_statement(&mut self) -> P<Stmt> {
        self.advance()?;
        let block = self.block()?;
        let handler = if self.eat("catch")? {
            self.expect("(")?;
            let name = self.ident()?;
            self.expect(")")?;
            Some((name, self.block()?))
        } else {
            None
        };
        let finalizer = if self.eat("finally")? {
            Some(self.block()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return self.fail("try without catch or finally");
        }
        Ok(Stmt::Try {
            block,
            handler,
            finalizer,
        })
    }

    /// Parameters and body, after the `function` keyword and any name.
    fn function(&mut self, name: Option<Name>, self_bind: bool) -> P<Arc<FuncDef>> {
        self.nest()?;
        let no_in = std::mem::replace(&mut self.no_in, false);
        let labels = std::mem::take(&mut self.labels);
        self.expect("(")?;
        let mut params = Vec::new();
        while !self.eat(")")? {
            params.push(self.ident()?);
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.ctx.push(FnCtx::default());
        let body = self.block();
        let mut ctx = self.ctx.pop().unwrap_or_default();
        let body = body?;
        self.no_in = no_in;
        self.labels = labels;
        self.depth -= 1;
        let taken: HashSet<&Name> = params.iter().collect();
        ctx.vars.retain(|v| !taken.contains(v));
        drop(taken);
        Ok(Arc::new(FuncDef {
            name,
            params,
            body,
            vars: ctx.vars,
            funcs: ctx.funcs,
            uses_args: ctx.uses_args,
            self_bind,
        }))
    }

    fn expression(&mut self) -> P<Expr> {
        let first = self.assign()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = vec![first];
        while self.eat(",")? {
            list.push(self.assign()?);
        }
        Ok(Expr::Seq(list))
    }

    fn assign(&mut self) -> P<Expr> {
        self.nest()?;
        let result = self.assign_inner();
        self.depth -= 1;
        result
    }

    fn assign_inner(&mut self) -> P<Expr> {
        let target = self.conditional()?;
        let op = match &self.tok {
            Tok::P("=") => None,
            Tok::P("+=") => Some(BinOp::Add),
            Tok::P("-=") => Some(BinOp::Sub),
            Tok::P("*=") => Some(BinOp::Mul),
            Tok::P("/=") => Some(BinOp::Div),
            Tok::P("%=") => Some(BinOp::Mod),
            Tok::P("&=") => Some(BinOp::BitAnd),
            Tok::P("|=") => Some(BinOp::BitOr),
            Tok::P("^=") => Some(BinOp::BitXor),
            Tok::P("<<=") => Some(BinOp::Shl),
            Tok::P(">>=") => Some(BinOp::Shr),
            Tok::P(">>>=") => Some(BinOp::UShr),
            _ => return Ok(target),
        };
        if !is_place(&target) {
            let Expr::Chain(head, mut links) = target else {
                return self.fail("cannot assign to this expression");
            };
            // `eq.gainLevels(band) = value` sets a COM property that takes
            // an argument. The host gets it as a call to `set:name`.
            let Some(Link::Call(mut arguments)) = links.pop() else {
                return self.fail("cannot assign to this expression");
            };
            let Some(Link::Member(name)) = links.pop() else {
                return self.fail("cannot assign to this expression");
            };
            self.advance()?;
            arguments.push(self.assign()?);
            links.push(Link::Member(format!("set:{name}").as_str().into()));
            links.push(Link::Call(arguments));
            return Ok(Expr::Chain(head, links));
        }
        self.advance()?;
        let value = self.assign()?;
        Ok(Expr::Assign(op, Box::new(target), Box::new(value)))
    }

    fn conditional(&mut self) -> P<Expr> {
        let test = self.binary(0)?;
        if !self.eat("?")? {
            return Ok(test);
        }
        let no_in = std::mem::replace(&mut self.no_in, false);
        let yes = self.assign()?;
        self.no_in = no_in;
        self.expect(":")?;
        let no = self.assign()?;
        Ok(Expr::Cond(Box::new([test, yes, no])))
    }

    fn operator(&self) -> Option<(u8, BinOp)> {
        let t = match &self.tok {
            Tok::P(t) => *t,
            Tok::Kw("in") if !self.no_in => return Some((6, BinOp::In)),
            Tok::Kw("instanceof") => return Some((6, BinOp::InstanceOf)),
            _ => return None,
        };
        Some(match t {
            "||" => (0, BinOp::Or),
            "&&" => (1, BinOp::And),
            "|" => (2, BinOp::BitOr),
            "^" => (3, BinOp::BitXor),
            "&" => (4, BinOp::BitAnd),
            "==" => (5, BinOp::Eq),
            "!=" => (5, BinOp::Ne),
            "===" => (5, BinOp::StrictEq),
            "!==" => (5, BinOp::StrictNe),
            "<" => (6, BinOp::Lt),
            ">" => (6, BinOp::Gt),
            "<=" => (6, BinOp::Le),
            ">=" => (6, BinOp::Ge),
            "<<" => (7, BinOp::Shl),
            ">>" => (7, BinOp::Shr),
            ">>>" => (7, BinOp::UShr),
            "+" => (8, BinOp::Add),
            "-" => (8, BinOp::Sub),
            "*" => (9, BinOp::Mul),
            "/" => (9, BinOp::Div),
            "%" => (9, BinOp::Mod),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8) -> P<Expr> {
        let mut left = self.unary()?;
        while let Some((level, mut op)) = self.operator() {
            if level < min {
                break;
            }
            self.advance()?;
            let mut rest = Vec::new();
            loop {
                self.nest()?;
                let right = self.binary(level + 1);
                self.depth -= 1;
                rest.push((op, right?));
                match self.operator() {
                    Some((l, next)) if l == level => {
                        op = next;
                        self.advance()?;
                    }
                    _ => break,
                }
            }
            left = Expr::Binary(Box::new(left), rest);
        }
        Ok(left)
    }

    fn unary(&mut self) -> P<Expr> {
        let op = match &self.tok {
            Tok::P("-") => UnOp::Neg,
            Tok::P("+") => UnOp::Plus,
            Tok::P("!") => UnOp::Not,
            Tok::P("~") => UnOp::BitNot,
            Tok::Kw("typeof") => UnOp::Typeof,
            Tok::Kw("void") => UnOp::Void,
            Tok::Kw("delete") => UnOp::Delete,
            Tok::P(p @ ("++" | "--")) => {
                let delta = if *p == "++" { 1.0 } else { -1.0 };
                self.advance()?;
                self.nest()?;
                let target = self.unary();
                self.depth -= 1;
                let target = target?;
                if !is_place(&target) {
                    return self.fail("cannot assign to this expression");
                }
                return Ok(Expr::Update {
                    prefix: true,
                    delta,
                    target: Box::new(target),
                });
            }
            _ => return self.postfix(),
        };
        self.advance()?;
        self.nest()?;
        let operand = self.unary();
        self.depth -= 1;
        Ok(Expr::Unary(op, Box::new(operand?)))
    }

    fn postfix(&mut self) -> P<Expr> {
        let target = self.chain(true)?;
        if let Tok::P(p @ ("++" | "--")) = &self.tok
            && !self.newline
            && is_place(&target)
        {
            let delta = if *p == "++" { 1.0 } else { -1.0 };
            self.advance()?;
            return Ok(Expr::Update {
                prefix: false,
                delta,
                target: Box::new(target),
            });
        }
        Ok(target)
    }

    /// A primary or `new` expression and its member and call links. `new`'s
    /// callee stops at the first call, whose arguments belong to the `new`.
    fn chain(&mut self, calls: bool) -> P<Expr> {
        let head = if self.is("new") {
            self.advance()?;
            self.nest()?;
            let callee = self.chain(false);
            self.depth -= 1;
            let callee = callee?;
            let args = if self.is("(") {
                self.arguments()?
            } else {
                Vec::new()
            };
            Expr::New(Box::new(callee), args)
        } else {
            self.primary()?
        };
        let mut links = Vec::new();
        loop {
            if self.eat(".")? {
                let name = match &self.tok {
                    Tok::Ident(name) => name.clone(),
                    // Keywords are fine as member names in JScript.
                    Tok::Kw(k) => {
                        let k: &str = k;
                        self.lexer.name(k)
                    }
                    _ => return self.fail("expected a member name"),
                };
                self.advance()?;
                links.push(Link::Member(name));
            } else if self.is("[") {
                self.advance()?;
                let no_in = std::mem::replace(&mut self.no_in, false);
                let index = self.expression()?;
                self.no_in = no_in;
                self.expect("]")?;
                links.push(Link::Index(index));
            } else if calls && self.is("(") {
                links.push(Link::Call(self.arguments()?));
            } else {
                break;
            }
        }
        if links.is_empty() {
            return Ok(head);
        }
        Ok(Expr::Chain(Box::new(head), links))
    }

    fn arguments(&mut self) -> P<Vec<Expr>> {
        self.expect("(")?;
        let no_in = std::mem::replace(&mut self.no_in, false);
        let mut args = Vec::new();
        while !self.is(")") {
            args.push(self.assign()?);
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.no_in = no_in;
        self.advance()?;
        Ok(args)
    }

    fn primary(&mut self) -> P<Expr> {
        let value = match self.tok.clone() {
            Tok::Num(n) => Expr::Num(n),
            Tok::Str(s) => Expr::Str(s),
            Tok::Ident(name) => {
                if (&*name == "arguments" || &*name == "eval")
                    && let Some(ctx) = self.ctx.last_mut()
                {
                    ctx.uses_args = true;
                }
                Expr::Ident(name)
            }
            Tok::Kw("this") => Expr::This,
            Tok::Kw("null") => Expr::Null,
            Tok::Kw("true") => Expr::Bool(true),
            Tok::Kw("false") => Expr::Bool(false),
            Tok::Kw("function") => return self.function_expression(),
            Tok::P("(") => {
                self.advance()?;
                let no_in = std::mem::replace(&mut self.no_in, false);
                let inner = self.expression()?;
                self.no_in = no_in;
                self.expect(")")?;
                return Ok(inner);
            }
            Tok::P("[") => return self.array_literal(),
            Tok::P("{") => return self.object_literal(),
            Tok::P("/" | "/=") => {
                let (body, flags) = self.lexer.regex(self.slash)?;
                Expr::Regex(body, flags)
            }
            Tok::Eof => return self.fail("unexpected end of script"),
            _ => return self.fail("unexpected token"),
        };
        self.advance()?;
        Ok(value)
    }

    fn function_expression(&mut self) -> P<Expr> {
        self.advance()?;
        let name = match &self.tok {
            Tok::Ident(_) => Some(self.ident()?),
            _ => None,
        };
        let named = name.is_some();
        let def = self.function(name, named)?;
        // JScript also declares a named function expression in the enclosing
        // scope.
        if named && let Some(ctx) = self.ctx.last_mut() {
            ctx.funcs.push(def.clone());
        }
        Ok(Expr::Func(def))
    }

    fn array_literal(&mut self) -> P<Expr> {
        self.advance()?;
        let no_in = std::mem::replace(&mut self.no_in, false);
        let mut items = Vec::new();
        while !self.is("]") {
            if self.eat(",")? {
                items.push(None);
                continue;
            }
            items.push(Some(self.assign()?));
            if !self.is("]") {
                self.expect(",")?;
            }
        }
        self.no_in = no_in;
        self.advance()?;
        Ok(Expr::Array(items))
    }

    fn object_literal(&mut self) -> P<Expr> {
        self.advance()?;
        let no_in = std::mem::replace(&mut self.no_in, false);
        let mut items = Vec::new();
        while !self.is("}") {
            let key = match &self.tok {
                Tok::Ident(name) | Tok::Str(name) => name.clone(),
                Tok::Num(n) => {
                    let text = number_to_string(*n);
                    self.lexer.name(&text)
                }
                Tok::Kw(k) => {
                    let k: &str = k;
                    self.lexer.name(k)
                }
                _ => return self.fail("expected a property name"),
            };
            self.advance()?;
            self.expect(":")?;
            items.push((key, self.assign()?));
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        self.no_in = no_in;
        self.advance()?;
        Ok(Expr::Object(items))
    }
}
