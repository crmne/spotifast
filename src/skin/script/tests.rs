use std::collections::HashMap;

use super::interp::MAX_STRING_BYTES;
use super::*;

/// A skin's world in miniature: `player` (1) with members, `scope` (2) owning
/// `width`, and `psPlaying`.
#[derive(Default)]
struct FakeHost {
    members: HashMap<(HostId, String), Value>,
    calls: Vec<String>,
}

impl Host for FakeHost {
    fn global(&mut self, name: &str) -> Option<Value> {
        match name {
            "player" => Some(Value::Host(1)),
            "psPlaying" => Some(Value::Number(3.0)),
            _ => None,
        }
    }
    fn has(&mut self, object: HostId, name: &str) -> bool {
        object == 2 && (name == "width" || name == "shout")
    }
    fn get(&mut self, object: HostId, name: &str) -> Value {
        let found = self.members.get(&(object, name.to_string())).cloned();
        found.unwrap_or(Value::Number(7.0))
    }
    fn set(&mut self, object: HostId, name: &str, value: Value) {
        self.members.insert((object, name.to_string()), value);
    }
    fn call(&mut self, object: HostId, name: &str, args: &[Value]) -> Result<Value, String> {
        self.calls.push(format!("{object}.{name}/{}", args.len()));
        if name == "fail" {
            return Err("host says no".into());
        }
        Ok(Value::Host(1))
    }
    fn index(&mut self, object: HostId, index: f64) -> Value {
        Value::Number(object as f64 * 100.0 + index)
    }
}

fn engine() -> Engine {
    Engine::new(Limits::default())
}

fn run(e: &mut Engine, source: &str) -> Result<Value, ScriptError> {
    let code = e.compile(source)?;
    e.run(code, None, &[], &mut FakeHost::default())
}

fn number(source: &str) -> f64 {
    match run(&mut engine(), source) {
        Ok(Value::Number(n)) => n,
        other => panic!("{source}: {other:?}"),
    }
}

fn text(source: &str) -> String {
    let mut e = engine();
    match run(&mut e, source) {
        Ok(v) => e.to_str(&v, &mut FakeHost::default()).to_string(),
        other => panic!("{source}: {other:?}"),
    }
}

/// After any limit, the engine still runs code and forgets the failure.
fn assert_alive(e: &mut Engine) {
    assert_eq!(run(e, "1+2"), Ok(Value::Number(3.0)));
    e.collect(&[]);
    assert_eq!(
        run(e, "'a'+'b'").map(|v| v == Value::Str("ab".into())),
        Ok(true)
    );
}

#[test]
fn engine_is_send() {
    fn check<T: Send>() {}
    check::<Engine>();
}

#[test]
fn numbers_print_as_ecmascript_says() {
    assert_eq!(text("0.1+0.2"), "0.30000000000000004");
    assert_eq!(text("1e21"), "1e+21");
    assert_eq!(text("1e20"), "100000000000000000000");
    assert_eq!(text("1.5e-7"), "1.5e-7");
    assert_eq!(text("0.000001"), "0.000001");
    assert_eq!(text("-0"), "0");
    assert_eq!(text("1/0"), "Infinity");
    assert_eq!(text("(255).toString(16)"), "ff");
    assert_eq!(text("(0.5).toString(2)"), "0.1");
    assert_eq!(text("(2.5).toFixed(0)"), "3");
    assert_eq!(text("(1.005).toFixed(2)"), "1.00");
    assert_eq!(text("(-1.5).toFixed(1)"), "-1.5");
    assert_eq!(text("(123.456).toPrecision(4)"), "123.5");
    assert_eq!(text("(1000).toExponential(1)"), "1.0e+3");
}

#[test]
fn operators() {
    assert_eq!(number("1+2*3-4/2"), 5.0);
    assert_eq!(number("7%3"), 1.0);
    assert_eq!(number("-1>>>28"), 15.0);
    assert_eq!(number("1<<33"), 2.0);
    assert_eq!(number("~5"), -6.0);
    assert_eq!(number("5&3|8^1"), 9.0);
    assert_eq!(text("1+'2'"), "12");
    assert_eq!(number("'3'*'4'"), 12.0);
    assert_eq!(text("null==undefined"), "true");
    assert_eq!(text("null===undefined"), "false");
    assert_eq!(text("'1'==1"), "true");
    assert_eq!(text("0||'x'"), "x");
    assert_eq!(text("1&&'y'"), "y");
    assert_eq!(text("typeof nothing"), "undefined");
    assert_eq!(text("typeof function(){}"), "function");
    assert_eq!(text("typeof player"), "object");
    assert_eq!(number("var a=1; a+=2; a<<=2; a"), 12.0);
    assert_eq!(number("var a=1; var b=a++ + ++a; b"), 4.0);
    assert_eq!(text("void 0"), "undefined");
    assert_eq!(number("(1,2,3)"), 3.0);
    assert_eq!(text("1<2?'y':'n'"), "y");
    assert_eq!(text("'a' in {a:1}"), "true");
    assert_eq!(text("[] instanceof Array"), "true");
    assert_eq!(text("var o={a:1}; delete o.a; o.a"), "undefined");
}

#[test]
fn closures_and_functions() {
    let counter = "function c(){var n=0;return function(){return ++n}}
        var a=c(),b=c(); a(); a(); a()+b()";
    assert_eq!(number(counter), 4.0);
    assert_eq!(
        number("function f(){return arguments.length} f(1,2,3)"),
        3.0
    );
    assert_eq!(number("g(); function g(){return 5} g()"), 5.0);
    assert_eq!(
        number("var f=function fact(n){return n<2?1:n*fact(n-1)}; f(5)"),
        120.0
    );
    assert_eq!(
        number("function P(x){this.x=x} P.prototype.get=function(){return this.x}; new P(4).get()"),
        4.0
    );
    assert_eq!(
        number("function A(){} A.prototype.v=1; function B(){} B.prototype=new A; new B().v"),
        1.0
    );
    assert_eq!(text("function A(){} new A instanceof A"), "true");
    assert_eq!(
        number("function f(a,b){return a+b} f.call(null,1,2)+f.apply(null,[3,4])"),
        10.0
    );
    assert_eq!(number("new Function('a','b','return a*b')(3,4)"), 12.0);
    assert_eq!(number("function f(a,a){return a} f(1,2)"), 2.0);
    assert_eq!(
        text("function f(){return this===g} var g=this;f.call(null)"),
        "true"
    );
    assert_eq!(
        number("var n=0;function f(){n++;return 'x'}'a'.replace('a',f);n"),
        1.0
    );
}

#[test]
fn automatic_semicolons() {
    assert_eq!(number("var a=1\nvar b=2\na+b"), 3.0);
    assert_eq!(text("function f(){return\n1}\nf()"), "undefined");
    assert_eq!(number("var a=1,b=2\na\n++b\nb"), 3.0);
    assert_eq!(number("do{}while(0) 5"), 5.0);
    assert_eq!(number("if(1){}\nvar x={a:1}\nx.a"), 1.0);
    assert_eq!(number("var i=0\nfor(;;){i++\nif(i>2)break}\ni"), 3.0);
    assert!(matches!(
        run(&mut engine(), "var a=1 var b"),
        Err(ScriptError::Syntax { .. })
    ));
}

#[test]
fn control_flow() {
    let fall = "var s='';switch(2){case 1:s+='a';case 2:s+='b';case 3:s+='c';break;case 4:s+='d'}s";
    assert_eq!(text(fall), "bc");
    assert_eq!(
        text("var s='';switch(9){case 1:s+='a';default:s+='d';case 2:s+='b'}s"),
        "db"
    );
    assert_eq!(
        text("var s='';switch(2){default:s+='d';case 2:s+='b'}s"),
        "b"
    );
    let labels = "var n=0;a:for(var i=0;i<3;i++){for(var j=0;j<3;j++){if(j==1)continue a;if(i==2)break a;n++}}n";
    assert_eq!(number(labels), 2.0);
    assert_eq!(number("var i=0;do{i++}while(i<3);i"), 3.0);
    assert_eq!(number("var n=0;a:{for(;;){n=1;break a}n=2}n"), 1.0);
    assert_eq!(text("var s='';var o={a:1,b:2};for(var k in o)s+=k;s"), "ab");
    assert_eq!(
        text("var s='';var a=[];a[2]=1;a.x=2;for(var k in a)s+=k;s"),
        "2x"
    );
    assert_eq!(
        text("Object.prototype.zz=1;var s='';for(var k in {a:1})s+=k;s"),
        "azz"
    );
    assert_eq!(number("var o={a:1};with(o){a=2;var b=a}o.a+b"), 4.0);
    assert_eq!(
        text("var s='';try{throw 'x'}catch(e){s+=e}finally{s+='f'}s"),
        "xf"
    );
    assert_eq!(number("function f(){try{return 1}finally{x=2}} f()+x"), 3.0);
    assert_eq!(
        number("function f(){for(;;){try{break}finally{}}return 1} f()"),
        1.0
    );
    assert_eq!(text("try{null.x}catch(e){e.name}"), "TypeError");
    assert_eq!(text("try{undefinedName}catch(e){e.name}"), "ReferenceError");
    assert_eq!(
        text("try{new ActiveXObject('x')}catch(e){'caught'}"),
        "caught"
    );
    assert_eq!(text("try{throw new Error('m')}catch(e){e.message}"), "m");
    assert_eq!(text("String(new TypeError('m'))"), "TypeError: m");
}

#[test]
fn strings_builtins() {
    assert_eq!(text("'abc'.charAt(1)"), "b");
    assert_eq!(number("'abc'.charCodeAt(2)"), 99.0);
    assert_eq!(number("'a\u{e9}\u{4e2d}'.length"), 3.0);
    assert_eq!(text("'a\u{e9}\u{4e2d}'.charAt(2)"), "\u{4e2d}");
    assert_eq!(text("'a\u{e9}\u{4e2d}'.substring(1,2)"), "\u{e9}");
    assert_eq!(number("'hello'.indexOf('l')"), 2.0);
    assert_eq!(number("'hello'.lastIndexOf('l')"), 3.0);
    assert_eq!(text("'hello'.substr(-3,2)"), "ll");
    assert_eq!(text("'hello'.slice(1,-1)"), "ell");
    assert_eq!(text("'hello'.substring(3,1)"), "el");
    assert_eq!(text("'a,b'.split(',')"), "a,b");
    assert_eq!(number("'abc'.split('').length"), 3.0);
    assert_eq!(text("'aXbXc'.split('X',2)"), "a,b");
    assert_eq!(text("'aB'.toUpperCase()+'aB'.toLowerCase()"), "ABab");
    assert_eq!(text("String.fromCharCode(72,105)"), "Hi");
    assert_eq!(text("'a'.concat('b',1)"), "ab1");
    assert_eq!(text("'x'.replace('x','[$&]')"), "[x]");
    let sprintf = "String.prototype.sprintf=function(){var s=this;for(var i=0;i<arguments.length;i++)s=s.replace('%s',arguments[i]);return s};'%s-%s'.sprintf(1,2)";
    assert_eq!(text(sprintf), "1-2");
}

#[test]
fn regular_expressions() {
    assert_eq!(text("/a(b+)c/.exec('xabbc')[1]"), "bb");
    assert_eq!(number("/b/.exec('abc').index"), 1.0);
    assert_eq!(text("/^\\d+$/.test('123')"), "true");
    assert_eq!(text("/^\\d+$/.test('12a')"), "false");
    assert_eq!(text("/HELLO/i.test('hello')"), "true");
    assert_eq!(text("/^b/m.test('a\\nb')"), "true");
    assert_eq!(text("'a1b22'.replace(/\\d+/g,'#')"), "a#b#");
    assert_eq!(
        text("'john smith'.replace(/(\\w+) (\\w+)/,'$2 $1')"),
        "smith john"
    );
    assert_eq!(
        text("'abc'.replace(/b/,function(m,o){return m.toUpperCase()+o})"),
        "aB1c"
    );
    assert_eq!(text("'a1b2'.match(/\\d/g)"), "1,2");
    assert_eq!(text("'a b  c'.split(/\\s+/)"), "a,b,c");
    assert_eq!(text("'a1b'.split(/(\\d)/)"), "a,1,b");
    assert_eq!(text("'ab'.split(/(?:)/)"), "a,b");
    assert_eq!(text("/a/.exec('xa').input"), "xa");
    assert_eq!(number("'abc'.search(/c/)"), 2.0);
    assert_eq!(text("/(a)|b/.exec('b')[1]"), "undefined");
    assert_eq!(text("/(a)\\1/.test('aa')"), "true");
    assert_eq!(text("/a+?/.exec('aaa')[0]"), "a");
    assert_eq!(text("/a{2,3}/.exec('aaaa')[0]"), "aaa");
    assert_eq!(text("/(?:ab)+/.exec('ababx')[0]"), "abab");
    assert_eq!(text("/a(?=b)/.test('ac')"), "false");
    assert_eq!(text("/a(?!b)/.test('ac')"), "true");
    assert_eq!(text("/\\bfoo\\b/.test('a foo b')"), "true");
    assert_eq!(text("/[^a-c]/.exec('abcd')[0]"), "d");
    assert_eq!(text("/(a*)*/.exec('b')[0]"), "");
    let looped = "var r=/a/g,n=0;while(r.exec('aXa'))n++;n";
    assert_eq!(number(looped), 2.0);
    assert_eq!(text("var r=/a/g;r.test('xa');r.lastIndex"), "2");
    assert_eq!(text("new RegExp('a+','g').source"), "a+");
    assert_eq!(text("String(/a/gi)"), "/a/gi");
    assert_eq!(text("try{new RegExp('(')}catch(e){e.name}"), "SyntaxError");
}

#[test]
fn arrays() {
    assert_eq!(number("new Array(5).length"), 5.0);
    assert_eq!(text("new Array(1,2)"), "1,2");
    assert_eq!(number("var a=[1,2,3];a.length=1;a[1]=5;a.length"), 2.0);
    assert_eq!(text("var a=[1];a[3]=2;a"), "1,,,2");
    assert_eq!(
        text("var a=[1,2];a.push(3);a.pop();a.shift();a.unshift(0);a"),
        "0,2"
    );
    assert_eq!(text("[1,2,3,4].slice(1,-1)"), "2,3");
    assert_eq!(
        text("var a=[1,2,3,4];var b=a.splice(1,2,9);a+'|'+b"),
        "1,9,4|2,3"
    );
    assert_eq!(text("[1].concat(2,[3,4])"), "1,2,3,4");
    assert_eq!(text("[1,2].join('-')"), "1-2");
    assert_eq!(text("[1,2,3].reverse()"), "3,2,1");
    assert_eq!(text("[3,10,2].sort()"), "10,2,3");
    assert_eq!(text("[3,10,2].sort(function(a,b){return a-b})"), "2,3,10");
    assert_eq!(text("[3,undefined,1].sort()"), "1,3,");
    assert_eq!(text("[[1,2],[3]]"), "1,2,3");
    // An inconsistent comparator neither panics nor loses elements.
    assert_eq!(
        number("[5,3,8,1,9,2,7].sort(function(){return Math.random()-0.5}).length"),
        7.0
    );
    assert_eq!(text("try{new Array(-1)}catch(e){e.name}"), "RangeError");
}

#[test]
fn other_builtins() {
    assert_eq!(
        number("Math.floor(1.7)+Math.ceil(1.2)+Math.round(2.5)+Math.abs(-1)"),
        7.0
    );
    assert_eq!(
        number("Math.max(1,3,2)+Math.min(1,3,2)+Math.pow(2,3)"),
        12.0
    );
    assert_eq!(text("var r=Math.random();r>=0&&r<1"), "true");
    assert_eq!(
        number("parseInt('42px')+parseInt('ff',16)+parseInt('0x10')+parseFloat('1.5e1x')"),
        328.0
    );
    assert_eq!(text("isNaN('x')+','+isFinite(1)"), "true,true");
    assert_eq!(text("escape('a b')+unescape('%41')"), "a%20bA");
    assert_eq!(text("encodeURIComponent('a b&\u{e9}')"), "a%20b%26%C3%A9");
    assert_eq!(text("decodeURIComponent('a%20%C3%A9')"), "a \u{e9}");
    assert_eq!(text("encodeURI('a b/c')+decodeURI('%2F')"), "a%20b/c%2F");
    assert_eq!(text("try{decodeURI('%')}catch(e){e.name}"), "URIError");
    assert_eq!(
        number("Number('12')+Number.MAX_VALUE/Number.MAX_VALUE"),
        13.0
    );
    assert_eq!(
        text("new Number(5)+new String('x')+new Boolean(false)"),
        "5xfalse"
    );
    assert_eq!(text("String(undefined)+Boolean('')"), "undefinedfalse");
    assert_eq!(text("({}).toString()"), "[object Object]");
    assert_eq!(text("({a:1}).hasOwnProperty('a')"), "true");
    assert_eq!(text("Object.prototype.toString.call([])"), "[object Array]");
}

#[test]
fn dates() {
    let d = "var d=new Date(2006,0,2,3,4,5,6);";
    assert_eq!(number(&format!("{d}d.getFullYear()")), 2006.0);
    assert_eq!(
        number(&format!("{d}d.getMonth()+d.getDate()+d.getHours()")),
        5.0
    );
    assert_eq!(
        number(&format!(
            "{d}d.getMinutes()+d.getSeconds()+d.getMilliseconds()"
        )),
        15.0
    );
    assert_eq!(number(&format!("{d}d.getDay()")), 1.0);
    assert_eq!(
        text(&format!("{d}String(d)")),
        "Mon Jan 2 03:04:05 UTC 2006"
    );
    assert_eq!(number(&format!("{d}d.setDate(32);d.getMonth()")), 1.0);
    assert_eq!(number("new Date(0).getTime()"), 0.0);
    assert_eq!(number("new Date(1704067200000).getFullYear()"), 2024.0);
    assert_eq!(number("new Date(2000,1,29).getDate()"), 29.0);
    assert_eq!(number("Date.parse('2006/01/02 00:00:01')"), 1136160001000.0);
    assert_eq!(text("typeof Date()"), "string");
    assert_eq!(text("new Date().getTime()>1e12"), "true");
}

#[test]
fn eval_uses_the_current_scope() {
    assert_eq!(
        number("function f(){var x=2;eval('x=x+1;var y=4');return x+y} f()"),
        7.0
    );
    assert_eq!(number("eval('1+2')"), 3.0);
    assert_eq!(number("eval(5)"), 5.0);
    assert_eq!(text("try{eval('(')}catch(e){e.name}"), "SyntaxError");
    let mut e = engine();
    let mut host = FakeHost::default();
    let code = e
        .compile("for(var i=0;i<2;i++)eval('player.left = ' + (i + 4) + ';');")
        .unwrap();
    e.run(code, None, &[], &mut host).unwrap();
    let left = host.members.get(&(1, "left".to_string()));
    assert_eq!(left, Some(&Value::Number(5.0)));
}

#[test]
fn handlers_resolve_names_in_order() {
    let mut e = engine();
    let mut host = FakeHost::default();
    e.load("var g=10;function OnLoad(){return g+1}", &mut host)
        .unwrap();
    let args = [("NewState", Value::Number(3.0))];
    let run_in = |e: &mut Engine, h: &mut FakeHost, s: &str| {
        let code = e.compile(s).unwrap();
        e.run(code, Some(2), &args, h)
    };
    assert_eq!(
        run_in(&mut e, &mut host, "NewState"),
        Ok(Value::Number(3.0))
    );
    assert_eq!(run_in(&mut e, &mut host, "width-1"), Ok(Value::Number(6.0)));
    assert_eq!(
        run_in(&mut e, &mut host, "OnLoad();"),
        Ok(Value::Number(11.0))
    );
    assert_eq!(
        run_in(&mut e, &mut host, "psPlaying"),
        Ok(Value::Number(3.0))
    );
    assert_eq!(run_in(&mut e, &mut host, "this"), Ok(Value::Host(2)));
    // Assignment reaches the owner, a new global, or nothing.
    run_in(&mut e, &mut host, "width=5;").unwrap();
    assert_eq!(
        host.members.get(&(2, "width".to_string())),
        Some(&Value::Number(5.0))
    );
    run_in(&mut e, &mut host, "fresh=1").unwrap();
    assert_eq!(run_in(&mut e, &mut host, "fresh"), Ok(Value::Number(1.0)));
    assert_eq!(
        run_in(&mut e, &mut host, "psPlaying=9;psPlaying"),
        Ok(Value::Number(3.0))
    );
    // A `var` stays in the handler.
    run_in(&mut e, &mut host, "var local=1;").unwrap();
    assert!(run_in(&mut e, &mut host, "local").is_err());
    // A bare call through the scope object goes to the host.
    run_in(&mut e, &mut host, "shout(1,2)").unwrap();
    assert_eq!(host.calls, ["2.shout/2"]);
}

#[test]
fn host_values_in_expressions() {
    let mut e = engine();
    let mut host = FakeHost::default();
    let mut go = |s: &str| {
        let code = e.compile(s).unwrap();
        e.run(code, None, &[], &mut host)
    };
    assert_eq!(go("player.volume"), Ok(Value::Number(7.0)));
    assert_eq!(go("player[3]"), Ok(Value::Number(103.0)));
    assert_eq!(go("player(4)"), Ok(Value::Number(104.0)));
    assert_eq!(go("player.play(1); player==player"), Ok(Value::Bool(true)));
    assert_eq!(go("String(player)"), Ok(Value::Str("[object]".into())));
    assert_eq!(go("isNaN(+player)"), Ok(Value::Bool(true)));
    assert_eq!(go("typeof player"), Ok(Value::Str("object".into())));
    assert_eq!(go("!player"), Ok(Value::Bool(false)));
    assert_eq!(
        go("try{player.fail()}catch(e){e.message}"),
        Ok(Value::Str("host says no".into()))
    );
    assert!(matches!(go("player.fail()"), Err(ScriptError::Thrown(_))));
    assert!(matches!(go("undefined.x()"), Err(ScriptError::Thrown(_))));
    go("var o={h:player};o.h.volume=2").unwrap();
    assert_eq!(
        host.members.get(&(1, "volume".to_string())),
        Some(&Value::Number(2.0))
    );
}

#[test]
fn completion_values() {
    assert_eq!(number("1+2"), 3.0);
    assert_eq!(number("3;if(1){4}"), 4.0);
    // A called function's statements are not the handler's.
    assert_eq!(number("function f(){5;return 6} 7;if(f()){}"), 7.0);
    assert_eq!(run(&mut engine(), "var a=1"), Ok(Value::Undefined));
    let mut e = engine();
    let code = e.compile("JScript: 2*2").unwrap();
    assert_eq!(e.compile("JScript: 2*2"), Ok(code));
    assert_eq!(
        e.run(code, None, &[], &mut FakeHost::default()),
        Ok(Value::Number(4.0))
    );
    assert_eq!(number("return 9;"), 9.0);
}

#[test]
fn load_and_call_entry_points() {
    let mut e = engine();
    let mut host = FakeHost::default();
    e.load("var x;var y=1;function add(a,b){return a+b}", &mut host)
        .unwrap();
    assert!(matches!(
        e.load("y=2;(", &mut host),
        Err(ScriptError::Syntax { .. })
    ));
    assert_eq!(
        e.call_global("add", &[Value::Number(1.0), Value::Number(2.0)], &mut host),
        Ok(Value::Number(3.0))
    );
    assert_eq!(run(&mut e, "y"), Ok(Value::Number(1.0)));
    assert_eq!(run(&mut e, "x"), Ok(Value::Undefined));
    assert!(matches!(
        e.call_global("x", &[], &mut host),
        Err(ScriptError::Thrown(_))
    ));
    assert!(matches!(
        e.call_global("gone", &[], &mut host),
        Err(ScriptError::Thrown(_))
    ));
    let f = run(&mut e, "(function(a){return a*2})").unwrap();
    assert_eq!(
        e.call_value(&f, &[Value::Number(4.0)], &mut host),
        Ok(Value::Number(8.0))
    );
    assert!(e.call_value(&Value::Number(1.0), &[], &mut host).is_err());
    assert_eq!(e.to_number(&Value::Str("12".into()), &mut host), 12.0);
    let looping = run(&mut e, "({toString:function(){return this+''}})").unwrap();
    assert_eq!(&*e.to_str(&looping, &mut host), "[object Object]");
    assert!(e.to_number(&looping, &mut host).is_nan());
}

#[test]
fn a_bug_in_a_skin_does_not_stop_the_file() {
    let mut e = engine();
    let mut host = FakeHost::default();
    let result = e.load(
        "var a=1;\nmissing();\nvar b=2;\nnull.x;\nfunction late(){return a+b}\nvar c=3;",
        &mut host,
    );
    assert_eq!(
        result,
        Err(ScriptError::Thrown(
            "ReferenceError: 'missing' is undefined".into()
        ))
    );
    assert_eq!(
        e.call_global("late", &[], &mut host),
        Ok(Value::Number(3.0))
    );
    assert_eq!(run(&mut e, "c"), Ok(Value::Number(3.0)));
    // A limit still stops the file, past any handler for it.
    let code = "try{for(;;){}}catch(e){}finally{escaped=1}\nvar after=1;";
    assert_eq!(e.load(code, &mut host), Err(ScriptError::Budget));
    assert_eq!(
        run(&mut e, "typeof after+typeof escaped"),
        Ok(Value::Str("undefinedundefined".into()))
    );
}

#[test]
fn names_fall_back_to_any_case() {
    let mut e = engine();
    let mut host = FakeHost::default();
    e.load("function StartVideo(){return 1} var Score=5;", &mut host)
        .unwrap();
    assert_eq!(run(&mut e, "Startvideo()"), Ok(Value::Number(1.0)));
    assert_eq!(run(&mut e, "score"), Ok(Value::Number(5.0)));
    assert_eq!(
        e.call_global("STARTVIDEO", &[], &mut host),
        Ok(Value::Number(1.0))
    );
    run(&mut e, "SCORE=6").unwrap();
    assert_eq!(run(&mut e, "Score"), Ok(Value::Number(6.0)));
    assert_eq!(run(&mut e, "typeof SCORE"), Ok(Value::Str("number".into())));
    // Builtins stay out of the fallback, so they do not shadow the host.
    assert!(run(&mut e, "date").is_err());
    assert_eq!(text("'aB'.toUppercase()"), "AB");
    assert_eq!(number("[1,2].Length"), 2.0);
    assert_eq!(number("'abc'.Length"), 3.0);
    assert_eq!(number("var o={loadPreference:4};o.loadpreference"), 4.0);
    assert_eq!(number("var o={a:1,A:2};o.a"), 1.0);
    assert_eq!(number("[1,2,3].Slice(1).length"), 2.0);
}

#[test]
fn collection_frees_and_keeps_the_right_things() {
    let mut e = engine();
    let mut host = FakeHost::default();
    let baseline = e.heap_bytes();
    e.load(
        "var keep={a:[1,2,{b:'x'}]};function f(){return keep.a[2].b}",
        &mut host,
    )
    .unwrap();
    run(
        &mut e,
        "for(var i=0;i<1000;i++){var o={n:i};o.self=o}o=null",
    )
    .unwrap();
    let held = run(
        &mut e,
        "(function(){var c=[7];return function(){return c[0]}})()",
    )
    .unwrap();
    let before = e.heap_bytes();
    e.collect(std::slice::from_ref(&held));
    assert!(e.heap_bytes() < before);
    assert!(e.heap_bytes() < baseline + 16_000, "{}", e.heap_bytes());
    assert_eq!(e.call_value(&held, &[], &mut host), Ok(Value::Number(7.0)));
    assert_eq!(
        e.call_global("f", &[], &mut host),
        Ok(Value::Str("x".into()))
    );
    // The freed slots come back, and a stale reference is an error, not a crash.
    e.collect(&[]);
    assert!(e.call_value(&held, &[], &mut host).is_err());
    assert_eq!(run(&mut e, "new Date(0).getTime()"), Ok(Value::Number(0.0)));
}

#[test]
fn calls_without_closures_leave_no_garbage() {
    let mut e = engine();
    run(&mut e, "function f(a){var b=a;return b} ").unwrap();
    let before = e.heap_bytes();
    run(&mut e, "for(var i=0;i<1000;i++)Math.abs(i)").unwrap();
    assert!(e.heap_bytes() - before < 2000);
}

/// A script cannot buy quadratic host work with long parameter or var lists.
#[test]
fn huge_declarations_pay_their_way() {
    let names: Vec<String> = (0..20_000).map(|i| format!("v{i}")).collect();
    let list = names.join(",");
    let source = format!("function f({list}){{var {list};return 1}} for(;;)f()");
    let mut e = engine();
    assert_eq!(run(&mut e, &source), Err(ScriptError::Budget));
    assert_alive(&mut e);
}

#[test]
fn step_budget_stops_every_kind_of_loop() {
    let bombs = [
        "while(1){}",
        "for(;;){}",
        "do{}while(1)",
        "function f(){for(;;);} f()",
        "var a=[];for(var i=0;i<3000;i++)a.push(i);for(;;)a.join(',')",
        "var a=[];for(var i=0;i<3000;i++)a.push(i%7);for(;;)a.sort()",
        "var s='';for(var i=0;i<20000;i++)s+='ab';for(;;)s.split('a')",
        "var s='';for(var i=0;i<10000;i++)s+='ab';for(;;)s.replace(/a/g,'bb')",
        "for(;;)eval('1')",
        "var o={};for(;;)try{o.x.y}catch(e){}",
        "var a=[];a.length=100000;for(;;)a.reverse()",
        "try{for(;;){}}catch(e){for(;;){}}",
        "function f(){try{for(;;);}finally{f()}} f()",
    ];
    for bomb in bombs {
        let mut e = engine();
        let result = run(&mut e, bomb);
        assert!(
            matches!(result, Err(ScriptError::Budget | ScriptError::Memory)),
            "{bomb}: {result:?}"
        );
        assert_alive(&mut e);
    }
}

#[test]
fn budget_resets_on_each_entry() {
    let mut e = engine();
    for _ in 0..3 {
        assert_eq!(run(&mut e, "for(;;){}"), Err(ScriptError::Budget));
        assert_eq!(
            run(&mut e, "var i=0;while(i<1000)i++;i"),
            Ok(Value::Number(1000.0))
        );
    }
}

#[test]
fn doubling_strings_stops_at_the_length_cap() {
    let mut e = engine();
    assert_eq!(run(&mut e, "s='ab';for(;;)s+=s"), Err(ScriptError::Memory));
    assert_alive(&mut e);
    assert!(
        run(&mut e, "s.length")
            .is_ok_and(|v| matches!(v, Value::Number(n) if n as usize <= MAX_STRING_BYTES))
    );
}

#[test]
fn heap_limit_stops_and_collection_recovers() {
    let limits = Limits {
        steps: 50_000_000,
        ..Limits::default()
    };
    for bomb in [
        "var a=[];for(;;)a.push({})",
        "var o={};for(var i=0;;i++)o['k'+i]=i",
        "var a=[];for(;;)a.push('x'+a.length)",
        "var a=[];a[4000000]=1",
        "new Array(4194304)",
        "function f(){var c=0;return function(){return c}} var a=[];for(;;)a.push(f())",
    ] {
        let mut e = Engine::new(Limits { ..limits });
        assert_eq!(run(&mut e, bomb), Err(ScriptError::Memory), "{bomb}");
        run(&mut e, "a=null;o=null").ok();
        e.collect(&[]);
        assert_alive(&mut e);
    }
    let mut e = engine();
    assert_eq!(run(&mut e, "new Array(4e9)"), Err(ScriptError::Memory));
    assert_eq!(run(&mut e, "[].length=4e9"), Err(ScriptError::Memory));
    assert_alive(&mut e);
}

#[test]
fn recursion_ends_in_a_depth_error() {
    let mut e = engine();
    assert_eq!(number("function f(n){return n==0?0:1+f(n-1)} f(10)"), 10.0);
    let bombs = [
        "function f(){return f()} f()",
        "function f(){try{f()}catch(e){f()}} f()",
        "var o={toString:function(){return ''+o}};''+o",
        "var o={valueOf:function(){return +o}};+o",
        "var a=[];a[0]=a;String(a)",
        "function f(){eval('f()')} f()",
        "function f(){[1,2].sort(f)} f()",
        "function f(){'a'.replace('a',f)} f()",
        "function f(){f.apply(null,[])} f()",
        "function f(){new f} f()",
    ];
    for bomb in bombs {
        let result = run(&mut e, bomb);
        assert!(
            matches!(result, Err(ScriptError::Depth | ScriptError::Budget)),
            "{bomb}: {result:?}"
        );
        assert_alive(&mut e);
    }
}

/// Runs `work` on a stack smaller than any a script meets: macOS gives a
/// secondary thread 512 KiB and Windows its main thread 1 MiB.
fn on_small_stack(work: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(work)
        .unwrap()
        .join()
        .unwrap();
}

fn survives(source: String) {
    on_small_stack(move || {
        let mut e = Engine::new(Limits {
            steps: 10_000_000,
            ..Limits::default()
        });
        let _ = run(&mut e, &source);
        assert_alive(&mut e);
    });
}

#[test]
fn parsing_fits_a_small_stack() {
    let n = 50_000;
    for (open, close) in [
        ("(", ")"),
        ("[", "]"),
        ("{", "}"),
        ("{a:", "}"),
        ("x={a:", "}"),
        ("!", ""),
        ("-", ""),
        ("typeof ", ""),
        ("++", ""),
        ("f(", ")"),
        ("a[", "]"),
        ("a?", ":1"),
        ("a=", ""),
        ("if(1)", ""),
        ("if(1);else ", ""),
        ("while(0)", ""),
        ("for(;;)", ""),
        ("do ", " while(0)"),
        ("try{", "}finally{}"),
        ("with(a)", ""),
        ("l:", ""),
        ("function f(){", "}"),
        ("(function(){", "})"),
        ("new ", ""),
    ] {
        survives(format!("{}1{}", open.repeat(n), close.repeat(n)));
    }
}

#[test]
fn evaluation_fits_a_small_stack() {
    for source in [
        "function f(){return f()} f()",
        "function f(n){var r=[{a:[1+(2*(3-(4+(n?f(n-1):0))))]}];return r} f(1000)",
        "function f(n){if(n){for(;;){try{{with({}){switch(1){case 1:return f(n-1)}}}}finally{}}}} f(1000)",
        "var o={}; o.f=function(){return this.f.call(this)}; o.f()",
        "function f(){return f.apply(null,[])} f()",
        "function F(){new F} new F",
        "function f(){return eval('f()')} f()",
        "function f(){return eval('((((((((((((((((f()))))))))))))))))')} f()",
    ] {
        survives(source.to_owned());
    }
}

#[test]
fn conversions_fit_a_small_stack() {
    for source in [
        "var o={toString:function(){return ''+o}}; ''+o",
        "var o={valueOf:function(){return +o}}; +o",
        "var o={toString:function(){return o}, valueOf:function(){return o}}; ''+o",
        "var a=[]; a[0]=a; a.join()",
        "var a=[]; a[0]=a; ''+a",
        "var a=[]; for(var i=0;i<100000;i++){a=[a]} ''+a",
        "function f(){return [[{toString:function(){return f()}}]].join()} f()",
    ] {
        survives(source.to_owned());
    }
}

#[test]
fn callbacks_fit_a_small_stack() {
    for source in [
        "function f(){return [1,2].sort(function(a,b){return f()})} f()",
        "function f(){return 'a'.replace(/a/,function(){return ''+{toString:f}})} f()",
    ] {
        survives(source.to_owned());
    }
}

#[test]
fn regular_expressions_fit_a_small_stack() {
    let n = 5_000;
    for pattern in [
        format!("{}a{}", "(".repeat(n), ")".repeat(n)),
        format!("{}a{}", "(?:".repeat(n), ")".repeat(n)),
        format!("{}a{}", "(?=".repeat(30), ")".repeat(30)),
        format!("{}a{}", "(?!".repeat(n), ")".repeat(n)),
    ] {
        survives(format!("new RegExp('{pattern}').test('a')"));
        survives(format!(
            "function f(n){{return n?[[[[f(n-1)]]]]:new RegExp('{pattern}').test('a')}} f(40)"
        ));
    }
}

#[test]
fn long_chains_of_objects_fit_a_small_stack() {
    on_small_stack(|| {
        let mut e = Engine::new(Limits {
            steps: 50_000_000,
            ..Limits::default()
        });
        // Long prototype and property chains: walked, marked, then dropped.
        let source = "var o={}; for(var i=0;i<50000;i++){function F(){}; F.prototype=o; o=new F}\
                      o.x; var l=null; for(var i=0;i<50000;i++){l={n:l}} 1";
        let _ = run(&mut e, source);
        e.collect(&[]);
        assert_alive(&mut e);
        drop(e);
    });
}

/// Eval reaches the parser and regex compiler with the stack already used,
/// so sweep how much is used when it does.
#[test]
fn every_depth_of_stack_is_safe_for_what_runs_on_it() {
    let parens = format!("{}1{}", "(".repeat(140), ")".repeat(140));
    let operators = "1||1&&1|1^1&1==1<1<<1+1*(".repeat(12);
    let tail = ")".repeat(12);
    let groups = format!("{}a{}", "(?=".repeat(30), ")".repeat(30));
    on_small_stack(move || {
        for depth in 1..40 {
            for last in [
                format!("eval('{parens}')"),
                format!("eval('{operators}1{tail}')"),
                format!("new RegExp('{groups}').test('a')"),
            ] {
                let source = format!("function f(n){{return n?[[f(n-1)]]:{last}}} f({depth})");
                let mut e = Engine::new(Limits::default());
                let _ = run(&mut e, &source);
            }
        }
    });
}

/// The allowance must leave ordinary skin code alone, even unoptimised.
#[test]
fn ordinary_call_depth_is_allowed() {
    on_small_stack(|| {
        let mut e = Engine::new(Limits::default());
        let source = "function f(n){if(n>0){return 1+f(n-1)}return 0} f(10)";
        assert_eq!(run(&mut e, source).unwrap(), Value::Number(10.0));
    });
}

#[test]
fn hostile_nesting_is_refused() {
    let n = 100_000;
    let mut e = engine();
    let sources = [
        "(".repeat(n),
        "[".repeat(n),
        "{".repeat(n),
        "!".repeat(n),
        "-".repeat(n * 2),
        "a=".repeat(n),
        "new ".repeat(n),
        "if(1)".repeat(n),
        "function f(){".repeat(n),
        "x?".repeat(n),
        "typeof ".repeat(n),
        "for(;;)".repeat(n),
        format!("{}1{}", "(".repeat(n), ")".repeat(n)),
    ];
    for source in &sources {
        assert!(e.compile(source).is_err());
        assert!(e.load(source, &mut FakeHost::default()).is_err());
    }
    assert_alive(&mut e);
    // Flat chains of any length are fine, and pay in steps.
    let plus = format!("a{}", "+a".repeat(n));
    let dots = format!("a{}", ".a".repeat(n));
    let calls = format!("a{}", "()".repeat(n));
    let elses = format!("if(0){{}}{}", "else if(0){}".repeat(n));
    for source in [&plus, &dots, &calls, &elses] {
        e.compile(source).unwrap();
    }
    let mut e = Engine::new(Limits {
        steps: 2_000_000,
        ..Limits::default()
    });
    assert_eq!(
        run(&mut e, &format!("var a=1;{plus}")),
        Ok(Value::Number(n as f64 + 1.0))
    );
    assert_alive(&mut e);
    assert!(e.compile(&format!("/{}/", "(".repeat(n))).is_ok());
    assert!(run(&mut e, &format!("/{}/", "(".repeat(n))).is_err());
    assert!(run(&mut e, &format!("new RegExp(new Array({n}).join('(a'))")).is_err());
    assert_alive(&mut e);
}

#[test]
fn regex_backtracking_is_budgeted() {
    let long = "var s='';for(var i=0;i<30;i++)s+='a';s+='!';";
    let bombs = [
        "/(a+)+$/.test(s)",
        "/(a|a)*$/.test(s)",
        "/(a*)*b/.test(s)",
        "/(.*)(.*)(.*)(.*)(.*)(.*)x/.test(s)",
        "/(a|aa)+$/.test(s)",
    ];
    for bomb in bombs {
        let mut e = engine();
        assert_eq!(
            run(&mut e, &format!("{long}{bomb}")),
            Err(ScriptError::Budget),
            "{bomb}"
        );
        assert_alive(&mut e);
    }
    let mut e = engine();
    for pattern in [
        "((a{1000}){1000}){1000}",
        "(?:){1000000000}",
        "(){4000000000}",
    ] {
        let huge = run(&mut e, &format!("new RegExp('{pattern}')"));
        assert!(matches!(huge, Err(ScriptError::Thrown(_))), "{pattern}");
    }
    // Compiling and class tests pay for their size, not just their count.
    let classes = "var c='[';for(var i=0;i<3000;i++)c+=String.fromCharCode(i+256);c+=']';\
        var r=new RegExp(c+c+c+c);for(;;)r.test('aaaaaaaaaaaaaaaaaaaaaaaa')";
    assert_eq!(run(&mut e, classes), Err(ScriptError::Budget));
    assert_eq!(
        run(&mut e, "for(var i=0;;i++)new RegExp('(a{1000}){60}'+i)"),
        Err(ScriptError::Budget)
    );
    assert_alive(&mut e);
}

#[test]
fn utf8_boundaries_do_not_panic() {
    assert_eq!(text("'\u{1f600}'.length"), "2");
    assert_eq!(text("'\u{1f600}x'.charAt(2)"), "x");
    assert_eq!(text("'\u{1f600}x'.substring(1)"), "\u{fffd}x");
    assert_eq!(text("'\u{e9}\u{e9}'.slice(1)"), "\u{e9}");
    assert_eq!(number("'\u{e9}a\u{e9}a'.lastIndexOf('a',2)"), 1.0);
    assert_eq!(text("'\u{e9}b'.replace(/b/,'c')"), "\u{e9}c");
    assert_eq!(text("'\\ud83d\\ude00'"), "\u{1f600}");
    assert_eq!(text("String.fromCharCode(0xd83d,0xde00)"), "\u{1f600}");
    assert_eq!(text("String.fromCharCode(0xd83d)"), "\u{fffd}");
    assert!(engine().compile("'\u{e9}").is_err());
    assert!(engine().compile("/\u{e9}").is_err());
    assert!(run(&mut engine(), "\u{e9}\u{4e2d}=1;\u{e9}\u{4e2d}").is_ok());
}

/// Mutates tiny programs with a fixed seed. Any panic, overflow or runaway
/// fails here, and the same inputs fail again.
#[test]
fn fuzz_mutated_programs() {
    const SEEDS: &[&str] = &[
        "function f(a,b){var c=a+b;return c*2}\nf(1,2);",
        "var o={a:1,'b':[1,2,3]};for(var k in o){o[k]=typeof k}",
        "try{null.x}catch(e){throw e}finally{1}",
        "var s='h\u{e9}llo'.replace(/(l+)/g,'[$1]').split('[');s.join('').substr(-2,1);",
        "switch(3){case 1:break;default:x=1;case 2:continue}",
        "a:for(var i=0;i<3;i++){while(1){continue a}}",
        "[3,1,2].sort(function(x,y){return y-x}).slice(1).concat([4]).reverse();",
        "new Date(2000,1,1).getDay()+Math.pow(2,0.5)+parseInt('z',36)+(1.5).toFixed(1);",
        "eval('var q=1;q<<2');with({v:1}){v=2};delete this.q;void 0;",
        "/^(a|b)*?[c-d]{1,2}\\b(?=x)\\1$/gi.exec('abcd');String.fromCharCode(65,0xd800);",
        "function P(){this.x=arguments.length}P.prototype.y=1;new P(1).y>>>1;",
        "escape('a b');unescape('%u0041');encodeURI('\u{e9}');decodeURIComponent('%E9');",
    ];
    const TOKENS: &[&str] = &[
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        ";",
        ",",
        ".",
        "=",
        "+",
        "-",
        "/",
        "*",
        "'",
        "\"",
        "\\",
        "\n",
        "function",
        "new",
        "var",
        "if",
        "for",
        "in",
        "typeof",
        "delete",
        "this",
        "null",
        "0",
        "1e308",
        "-1",
        "4294967295",
        "9e99",
        "NaN",
        "\u{e9}",
        "\u{1f600}",
        "arguments",
        "eval",
        "length",
        "prototype",
        "toString",
        "valueOf",
        "while(1)",
        "/a*/g",
        ".length=1e9",
        ".call",
        ".apply",
        "::",
        "$1",
        "[[",
        "}}",
        "))",
        "?",
        ":",
        "<<",
        ">>>",
        "%",
        "\u{0}",
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut random = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };
    let mut host = FakeHost::default();
    let mut e = Engine::new(Limits {
        steps: 3_000,
        heap_bytes: 1 << 20,
        max_depth: 32,
    });
    for round in 0..6_000 {
        let mut chars: Vec<String> = SEEDS[random(SEEDS.len())]
            .chars()
            .map(String::from)
            .collect();
        for _ in 0..=random(5) {
            let at = random(chars.len() + 1);
            match random(4) {
                0 if at < chars.len() => {
                    chars.remove(at);
                }
                1 if at < chars.len() => chars[at] = TOKENS[random(TOKENS.len())].to_string(),
                2 => {
                    let other = SEEDS[random(SEEDS.len())];
                    let cut = other.char_indices().map(|(i, _)| i).nth(random(20));
                    chars.insert(at, other[..cut.unwrap_or(0)].to_string());
                }
                _ => chars.insert(at, TOKENS[random(TOKENS.len())].to_string()),
            }
        }
        let source = chars.concat();
        if let Ok(code) = e.compile(&source) {
            let _ = e.run(code, Some(2), &[("arg", Value::Number(1.0))], &mut host);
        }
        let _ = e.load(&source, &mut host);
        if round % 64 == 0 {
            e.collect(&[]);
        }
    }
    e.collect(&[]);
    assert_eq!(run(&mut e, "1+1"), Ok(Value::Number(2.0)));
}
