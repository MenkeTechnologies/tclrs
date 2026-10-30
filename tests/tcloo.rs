//! TclOO (`crate::oo`): `oo::class`, `oo::define`, `oo::objdefine`, `oo::copy`,
//! `self`, `my`, `next`, `nextto` and `info object` / `info class`. Every
//! expectation here is the output of the same script under tclsh 9.1.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

/// Instances keep their own declared variables; `destroy` and `rename obj {}` run the destructor once and take the command away.
#[test]
fn a_class_has_a_constructor_methods_declared_variables_and_a_destructor() {
    let script = r#"oo::class create Counter {
    variable count
    constructor {{start 0}} { set count $start }
    method incr {{by 1}} { incr count $by }
    method get {} { return $count }
    destructor { puts "bye [self]" }
}
set c [Counter new 5]
Counter create c2
puts "$c [$c incr] [$c incr 10] [c2 incr] [c2 get] [$c get]"
puts [info object class $c]
puts [lsort [info object methods $c -all]]
puts [info class instances Counter]
$c destroy
puts [list [info commands $c] [info object isa object $c]]
rename c2 {}
puts [list [info commands c2]]
"#;
    let expected = r#"::oo::Obj22 6 16 1 1 16
::Counter
destroy get incr
::oo::Obj22 ::c2
bye ::oo::Obj22
{} 0
bye ::c2
{}
"#;
    assert_eq!(run(script), expected);
}

/// `next` reaches the overridden implementation, `nextto` skips ahead to a named class, and `self` reports the class, method and next step.
#[test]
fn next_and_nextto_walk_the_inheritance_chain() {
    let script = r#"oo::class create Animal {
    variable name
    constructor {n} { set name $n }
    method speak {} { return "[my sound] from $name" }
    method sound {} { return ... }
    method who {} { return [list [self class] [self method] [self next]] }
}
oo::class create Dog {
    superclass Animal
    constructor {n} { next $n }
    method sound {} { return Woof }
    method speak {} { return "Dog: [next]" }
}
oo::class create Puppy {
    superclass Dog
    method speak {} { return "Puppy: [nextto Animal]" }
}
set d [Dog new Rex]
puts [$d speak]
puts [[Puppy new Bit] speak]
puts [$d who]
puts [info class superclasses Puppy]
puts [info class subclasses Animal]
puts [list [info object isa typeof $d Animal] [info object isa class Dog] [info object isa metaclass Dog]]
puts [info object call $d speak]
puts [info class call Puppy sound]
"#;
    let expected = r#"Dog: Woof from Rex
Puppy: Woof from Bit
::Animal who {}
::Dog
::Dog
1 1 0
{method speak ::Dog method} {method speak ::Animal method}
{method sound ::Dog method} {method sound ::Animal method}
"#;
    assert_eq!(run(script), expected);
}

/// Method and constructor usages name the command as the call spelt it, a forward's usage names the forward, and an unexported method is not listed to an outside caller.
#[test]
fn refusals_are_worded_as_tclsh_words_them() {
    let script = r#"oo::class create E {constructor {a} {set ::e $a}}
oo::class create F {
    superclass E
    constructor {} { next 5 }
    method two {a {b 2} args} { list $a $b $args }
    method Hidden {} { return hidden }
    method shown {} { my Hidden }
    forward fw my two
}
set o [F new]
foreach script {
    {E new}
    {$o}
    {$o nosuch}
    {$o Hidden}
    {$o two}
    {$o fw}
    {$o shown 1}
    {oo::object create {}}
    {oo::class create}
    {oo::define F method}
    {oo::define Nope {}}
    {F create F}
    {self}
    {next}
} {
    puts [list [catch $script m] $m]
}
puts [$o shown]
puts [$o two 1]
puts [$o fw 1 2 3]
"#;
    let expected = r#"1 {wrong # args: should be "E new a"}
1 {wrong # args: should be "::oo::Obj24 method ?arg ...?"}
1 {unknown method "nosuch": must be destroy, fw, shown or two}
1 {unknown method "Hidden": must be destroy, fw, shown or two}
1 {wrong # args: should be "::oo::Obj24 two a ?b? ?arg ...?"}
1 {wrong # args: should be "::oo::Obj24 fw a ?b? ?arg ...?"}
1 {wrong # args: should be "::oo::Obj24 shown"}
1 {object name must not be empty}
1 {wrong # args: should be "oo::class create objectName ?arg ...?"}
1 {wrong # args: should be "method name ?option? args body"}
1 {Nope does not refer to an object}
1 {can't create object "F": command already exists with that name}
1 {invalid command name "self"}
1 {invalid command name "next"}
hidden
1 2 {}
1 2 3
"#;
    assert_eq!(run(script), expected);
}

/// An object or class mixin's method comes first and `next` reaches the class's; a filter sees every call and `self target` names the method it wraps.
#[test]
fn mixins_and_filters_join_the_call_chain() {
    let script = r#"oo::class create Loud { method speak {} { return [string toupper [next]] } }
oo::class create Base { method speak {} { return base } }
set o [Base new]
oo::objdefine $o mixin Loud
puts [$o speak]
puts [info object mixins $o]
puts [info object call $o speak]
oo::objdefine $o mixin -clear
puts [$o speak]
oo::define Base mixin Loud
puts [[Base new] speak]
puts [list [catch {oo::define Base mixin Loud Loud} m] $m]
oo::class create Logger {
    variable log
    constructor {} { set log {} }
    filter tracer
    method tracer args { lappend log [self target]; next {*}$args }
    method a {} { my b; return a }
    method b {} { return b }
    method log {} { return $log }
}
set l [Logger new]
$l a
puts [$l log]
"#;
    let expected = r#"BASE
::Loud
{method speak ::Loud method} {method speak ::Base method}
base
BASE
1 {class should only be a direct mixin once}
{::Logger a} {::Logger b} {::Logger log}
"#;
    assert_eq!(run(script), expected);
}

/// Destroying a class takes its subclasses and instances with it, last first; a failed constructor runs the destructor; deleting an object's namespace destroys the object.
#[test]
fn destruction_reaches_subclasses_instances_and_nested_objects() {
    let script = r#"oo::class create A { destructor { puts "A dtor [self]" } }
oo::class create B { superclass A; destructor { puts "B dtor [self]"; next } }
A create a1
A create a2
B create b1
A destroy
puts [list [info commands a1] [info commands b1] [info commands B]]
oo::class create C { constructor {} { error ctorfail }; destructor { puts "C dtor" } }
puts [list [catch {C create cc} m] $m [info commands cc]]
oo::class create D { destructor { error dtorfail } }
D create d1
puts [list [catch {d1 destroy} m] $m [info commands d1]]
D create d2
puts [list [catch {rename d2 {}} m] $m [info commands d2]]
oo::class create N {
    variable n
    method make {i} { set n $i; [self class] create child }
    destructor { lappend ::order $n }
}
set ::order {}
set base [N new]
set obj $base
for {set i 1} {$i < 4} {incr i} { set obj [$obj make $i] }
$base destroy
puts $::order
oo::class create Z { destructor { puts "Z gone" } }
set z [Z new]
namespace delete [info object namespace $z]
puts [info commands $z]
"#;
    let expected = r#"B dtor ::b1
A dtor ::b1
A dtor ::a2
A dtor ::a1
{} {} {}
C dtor
1 ctorfail {}
1 dtorfail {}
0 {} {}
1 2 3
Z gone

"#;
    assert_eq!(run(script), expected);
}

/// `errorInfo` carries `(class "::C" method "m" line 1)` and `(in definition script for class "::C" line 3)` where tclsh's does.
#[test]
fn errorinfo_names_the_method_and_the_definition_script() {
    let script = r#"catch {oo::class create Ab {
    method a {} {}
    bogus
}}
puts $::errorInfo
oo::class create Er { method m {} { error inner } }
catch {[Er new] m}
puts $::errorInfo
oo::class create Ctor { constructor {} { error x } }
catch {Ctor new}
puts $::errorInfo
"#;
    let expected = r#"invalid command name "bogus"
    while executing
"bogus"
    (in definition script for class "::Ab" line 3)
    invoked from within
"oo::class create Ab {
    method a {} {}
    bogus
}"
inner
    while executing
"error inner "
    (class "::Er" method "m" line 1)
    invoked from within
"[Er new] m"
x
    while executing
"error x "
    (class "::Ctor" constructor line 1)
    invoked from within
"Ctor new"
"#;
    assert_eq!(run(script), expected);
}

/// `oo::copy` runs no constructor; `<cloned>` is called with the source's name and the source's variables are copied.
#[test]
fn oo_copy_calls_cloned_and_copies_the_namespace_variables() {
    let script = r#"oo::class create Foo {
    constructor {msg} { variable v $msg }
    method <cloned> {from} { next $from; lappend ::result cloned $from }
    method check {} { variable v; return $v }
}
Foo create foo ok
oo::copy foo bar
puts [list $::result [foo check] [bar check] [info object class bar]]
"#;
    let expected = r#"{cloned ::foo} ok ok ::Foo
"#;
    assert_eq!(run(script), expected);
}

/// `my variable`, `variable`, a class's `variable` declaration and `namespace current` all reach the object the method was called on, so one compiled body serves every instance.
#[test]
fn method_bodies_reach_the_running_objects_namespace() {
    let script = r#"oo::class create P {
    variable decl
    method a {} { my variable v; set v 7; return [my b] }
    method b {} { my variable v; return "v=$v" }
    method ns {} { namespace current }
    method cnt {} { variable k; incr k }
    method set {x} { set decl $x }
    method get {} { return $decl }
}
set p [P new]
set q [P new]
puts [$p a]
puts [expr {[$p ns] eq [info object namespace $p]}]
puts "[$p cnt][$p cnt][$q cnt]"
$p set one; $q set two
puts "[$p get] [$q get]"
puts [lsort [info object vars $p]]
puts [set [info object namespace $q]::decl]
oo::objdefine $p export varname eval
puts [expr {[$p varname decl] eq "[info object namespace $p]::decl"}]
puts [$p eval {set decl}]
"#;
    let expected = r#"v=7
1
121
one two
decl k v
two
1
one
"#;
    assert_eq!(run(script), expected);
}

/// `return -code break` from a method, or from a procedure defined in another chunk, breaks the caller's loop; `info commands ::a::*` lists only `::a`'s commands; `rename` finds an unqualified name in the global namespace.
#[test]
fn fixes_that_came_with_tcloo() {
    let script = r#"oo::class create R {
    method br {} { return -code break }
    method early {x} { if {$x} { return yes }; return no }
}
set r [R new]
set n 0
foreach i {1 2 3} { incr n; if {$i == 2} { $r br } }
puts "$n [$r early 1] [$r early 0]"
eval {proc stop {} {return -code break}}
set n 0
foreach i {1 2 3} { incr n; if {$i == 2} { stop } }
puts $n
namespace eval a { proc p {} {}; namespace eval b { proc q {} {} } }
puts [info commands ::a::*]
proc g {} {return g}
namespace eval x { rename g h }
puts [list [info commands ::x::*] [x::h] [info commands g]]
"#;
    let expected = r#"2 yes no
2
::a::p
::x::h g {}
"#;
    assert_eq!(run(script), expected);
}

/// Slot operations, `deletemethod`, `renamemethod`, their refusals, `info class definition`/`methodtype`/`forward`, and an object turned into a class by `oo::objdefine … class oo::class`.
#[test]
fn definitions_and_introspection_agree_with_tclsh() {
    let script = r#"oo::class create V {
    variable a b
    method m {} {}
    method x {} {}
}
oo::define V variable -append c
puts [info class variables V]
oo::define V variable -remove a
puts [info class variables V]
oo::define V deletemethod x
oo::define V renamemethod m n
puts [lsort [info class methods V]]
puts [list [catch {oo::define V superclass -foo} m] $m]
puts [list [catch {oo::define V superclass V} m] $m]
puts [list [catch {oo::define V variable a::b} m] $m]
puts [list [catch {oo::define V renamemethod n n} m] $m]
puts [list [catch {oo::define V deletemethod zz} m] $m]
oo::define V method d {a {b 2}} {return $a$b}
puts [info class definition V d]
puts [info class methodtype oo::object destroy]
oo::define V forward fw string length
puts [info class forward V fw]
oo::object create fooObj
oo::objdefine fooObj class oo::class
oo::define fooObj method x {} {expr {1+2+3}}
puts [[fooObj new] x]
"#;
    let expected = r#"a b c
b c
n
1 {unknown method "-foo": must be -append, -appendifnew, -clear, -prepend, -remove or -set}
1 {attempt to form circular dependency graph}
1 {invalid declared variable name "a::b": must not contain namespace separators}
1 {cannot rename method to itself}
1 {method zz does not exist}
{a {b 2}} {return $a$b}
core method: "destroy"
string length
6
"#;
    assert_eq!(run(script), expected);
}

/// `oo::singleton`, `oo::SingletonInstance` and `oo::abstract`, the classes
/// `MakeAdditionalClasses` adds: one instance per singleton class, which can be
/// neither destroyed nor copied, and an abstract class with no `new` or
/// `create` of its own while its subclasses keep them.
#[test]
fn singleton_and_abstract_metaclasses() {
    let script = r#"foreach c {oo::singleton oo::abstract oo::SingletonInstance} {puts "$c [info object namespace $c] [info object creationid $c] [info class superclasses $c] [lsort [info class methods $c -private]]"}
puts [info object isa metaclass oo::singleton][info object isa metaclass oo::abstract][info object isa metaclass oo::SingletonInstance]
oo::singleton create S {method v {} {return v}}
set o [S new]
puts $o
puts [info object mixins $o]|[info object methods $o -all]|[info object class $o]
puts [catch {$o destroy} m]$m|$::errorCode
puts [catch {S create k} m]$m
puts [catch {oo::copy $o} m]$m|$::errorCode
puts [expr {[S new] eq $o}]
puts [$o v]
oo::abstract create A {method hi {} {return hi}}
puts [catch {A new} m]$m|$::errorCode
puts [catch {A create x} m]$m
puts [info object class A]|[info class superclasses A]
oo::class create B {superclass A}
set b [B new]
puts "[info object class $b] [$b hi]"
puts [catch {oo::singleton new} m]$m
puts [catch {oo::abstract} m]$m
puts [lsort [info class instances oo::singleton]]
puts [lsort [info class instances oo::SingletonInstance]]
puts [namespace exists ::oo::Obj15][namespace which oo::abstract]
oo::class create P
puts [P new]
oo::singleton create T
oo::define T constructor {x} {puts ctor:$x}
puts [catch {T new} m]$m
puts [T new 1]
puts [T new 2]
S destroy
puts [info commands $o]x
"#;
    let expected = r#"oo::singleton ::oo::Obj15 15 ::oo::class new
oo::abstract ::oo::Obj17 17 ::oo::class 
oo::SingletonInstance ::oo::Obj16 16 ::oo::object <cloned> destroy
110
::oo::Obj22
::oo::SingletonInstance|destroy v|::S
1may not destroy a singleton object|TCL OO SINGLETON
1unknown method "create": must be destroy or new
1may not clone a singleton object|TCL OO SINGLETON
1
v
1unknown method "new": must be destroy|TCL LOOKUP METHOD new
1unknown method "create": must be destroy
::oo::abstract|::oo::object
::B hi
0::oo::Obj29
1wrong # args: should be "oo::abstract method ?arg ...?"
::S ::oo::Obj29
::oo::Obj22
1::oo::abstract
::oo::Obj33
1wrong # args: should be "T new x"
ctor:1
::oo::Obj37
::oo::Obj37
x
"#;
    assert_eq!(run(script), expected);
}

/// `info level` inside a method names the command that invoked it — `o m 1`,
/// `next 1`, `my n`, `C create o 1`, the empty list for a destructor — as
/// `InfoLevelCmd` reads `objv`, never the procedure the body compiled to; and
/// `-errorstack`'s `CALL` element agrees.
#[test]
fn info_level_names_the_command_that_invoked_a_method() {
    let script = r#"oo::class create A {method m {a} {return "A:[info level 0]"}}
oo::class create C {superclass A; method m {a {b 2}} {return "[info level 0]|[next $a]|[my n]"}; method n {} {info level 0}; constructor {x} {puts "ctor [info level 0]"}}
C create o 1
puts [o m 1]
set p [C new 2]
puts [$p m 5 6]
proc q {} {o m 7}
puts [q]
oo::objdefine o method z args {list [info level] [info level 0]}
puts [o z 1 2]
puts [::o z]
A destroy
oo::class create A {method m {a} {return "A:[info level 0]"}}
oo::class create C {superclass A
  method m {a} {return "[info level 0]|[nextto A $a]"}
  method unknown {name args} {return "unk:[info level 0]"}
  destructor {puts "dtor:[info level 0]"}
  method e {} {error boom}
  method f {} {proc inner {} {return [info level 1]}; inner}
}
C create o
puts [o m 3]
puts [o zork 1 2]
puts [o f]
catch {o e} m opts
puts [dict get $opts -errorstack]
o destroy
oo::class create F {method g {x} {return g:[info level 0]}; filter flt; method flt args {return "flt:[info level 0]:[next {*}$args]"}}
F create f
puts [f g 9]
"#;
    let expected = r#"ctor C create o 1
o m 1|A:next 1|my n
ctor C new 2
::oo::Obj25 m 5 6|A:next 5|my n
o m 7|A:next 7|my n
1 {o z 1 2}
1 {::o z}
o m 3|A:nextto A 3
unk:o zork 1 2
o f
INNER {returnImm boom {}} CALL {o e}
dtor:
flt:f g 9:g:next 9
"#;
    assert_eq!(run(script), expected);
}

/// The `oo::Helpers` commands inside a method — `callback` / `mymethod` naming the
/// object's `my`, `classvariable` linking to the declaring class's namespace,
/// `link` making commands that call `my` — and every object's `myclass`; with
/// the refusals `TclOOCallbackObjCmd`, `TclOOClassVariableObjCmd` and
/// `TclOOLinkObjCmd` raise.
#[test]
fn oo_helpers_callback_classvariable_link_and_myclass() {
    let script = r#"oo::class create C {
  method cb {} {callback m 1 2}
  method mm {} {mymethod m 3}
  method m args {return "m:$args"}
  method cv {} {classvariable count; incr count}
  method ln {} {link m {alias m}; return [list [m a] [alias b]]}
  method mc {} {myclass foo}
  self method foo {} {return foo}
}
C create o
set cb [o cb]
puts [llength $cb]:[lindex $cb 1]:[lrange $cb 2 end]
puts [{*}$cb]
puts [{*}[o mm]]
puts [o cv][o cv]
puts [set [info object namespace C]::count]
puts [lindex $cb 0]|[info object namespace C]|[info object namespace o]
puts [info vars [info object namespace C]::*]
puts [o ln]
puts [lsort [info commands [info object namespace o]::*]]
puts [o mc]
puts [catch {callback x} m]$m|$::errorCode
puts [catch {classvariable x} m]$m
puts [catch {link x} m]$m
puts [catch {myclass} m]$m
puts [lsort [info commands ::oo::Helpers::*]]
oo::objdefine o method cv2 {} {classvariable q}
puts [catch {o cv2} m]$m|$::errorCode
oo::define C method bad {} {classvariable a(b)}
puts [catch {o bad} m]$m|$::errorCode
oo::define C method bad2 {} {link {a b c}}
puts [catch {o bad2} m]$m|$::errorCode
o destroy
puts [info commands ::oo::Obj23::m]x
puts [catch {::oo::Helpers::callback x} m]$m|$::errorCode
puts [catch {::oo::Helpers::classvariable} m]$m
oo::class create K {
  method add {v} {classvariable items; lappend items $v; return [llength $items]}
  method gl {} {link {::gk show}; return ok}
  method show {} {return "show:[self]"}
  method mc {} {myclass create kk}
  method cbn {} {callback}
  constructor {} {classvariable n; incr n}
}
set a [K new]; set b [K new]
puts [$a add 1][$b add 2][$a add 3]
puts [set [info object namespace K]::n]
$a gl
puts [gk]
puts [catch {$a mc} m]$m
puts [catch {$a cbn} m]$m
puts [lsort [info commands [info object namespace $a]::*]]
puts [lsort [info commands ::oo::Obj1::*]]
$a destroy
puts [info commands ::gk]x
"#;
    let expected = r#"4:m:1 2
m:1 2
m:3
12
2
::oo::Obj22::my|::oo::Obj20|::oo::Obj22
::oo::Obj20::count
m:a m:b
::oo::Obj22::alias ::oo::Obj22::m ::oo::Obj22::my ::oo::Obj22::myclass
foo
1invalid command name "callback"|TCL LOOKUP COMMAND callback
1invalid command name "classvariable"
1invalid command name "link"
1invalid command name "myclass"
::oo::Helpers::callback ::oo::Helpers::classvariable ::oo::Helpers::link ::oo::Helpers::mymethod ::oo::Helpers::next ::oo::Helpers::nextto ::oo::Helpers::self
1method not defined by a class|TCL OO UNMATCHED_CONTEXT
1bad variable name "a(b)": can't create a scalar variable that looks like an array element|TCL UPVAR LOCAL_ELEMENT
1bad link description; must only have one or two elements|TCL OO CMDLINK_FORMAT
x
1::oo::Helpers::callback may only be called from inside a method|TCL OO CONTEXT_REQUIRED
1wrong # args: should be "::oo::Helpers::classvariable name ..."
123
2
show:::oo::Obj25
0::oo::Obj25::kk
1wrong # args: should be "callback method ..."
::oo::Obj25::kk ::oo::Obj25::my ::oo::Obj25::myclass
::oo::Obj1::my ::oo::Obj1::myclass
x
"#;
    assert_eq!(run(script), expected);
}

/// TIP 500 private variables: a method sees the private variables of the class
/// or object that declared it, kept in the object's namespace as `"<creation
/// id> : <name>"` (`PRIVATE_VARIABLE_PATTERN`), so two classes' `x` are two
/// variables; `my varname` answers the mangled name in a method that can see it,
/// and `info object vars` lists them — written back before it reads.
#[test]
fn private_variables_are_kept_under_the_declarers_creation_id() {
    let script = r#"oo::class create A {private variable x; method seta v {set x $v}; method geta {} {return $x}}
oo::class create B {superclass A; private variable x; variable y; method setb v {set x $v}; method getb {} {return $x}}
B create o
o seta 1; o setb 2
puts [o geta][o getb]
puts [lsort [info vars [info object namespace o]::*]]
puts [info object creationid A][info object creationid B][info object creationid o]
oo::objdefine o {private variable z; method setz v {set z $v; return [my varname z]}}
puts [o setz 5]
puts [lsort [info object vars o]]
oo::define A method vn {} {my varname x}
puts [o vn]
oo::define A method ev {} {my eval {info exists x}}
puts [o ev]
oo::define A method mv {} {my variable x; return [info exists x]}
puts [o mv]
oo::copy o o2
puts [lsort [info object vars o2]]
oo::class create M {private variable q; method setq v {set q $v}; method vq {} {my varname q}}
oo::objdefine o mixin M
o setq 3
puts [lsort [info object vars o]]
puts [o vq]
oo::class create P {private variable x; variable x; method seta v {set x $v}; method geta {} {return $x}}
oo::class create Q {superclass P; variable x; method getq {} {return [info exists x]}}
Q create p; p seta 7
puts [p geta][p getq]
oo::class create R {private {variable p q}; variable s; method sp {} {set p 1; set q 2; set s 3; lsort [info object vars [self]]}}
puts [[R new] sp]
puts [info class variables R -private]
"#;
    let expected = r#"12
{::oo::Obj24::20 : x} {::oo::Obj24::22 : x}
202224
::oo::Obj24::24 : z
{20 : x} {22 : x} {24 : z}
::oo::Obj24::20 : x
0
1
{20 : x} {22 : x} {24 : z}
{20 : x} {22 : x} {24 : z} {26 : q}
::oo::Obj24::26 : q
70
{33 : p} {33 : q} s
p q
"#;
    assert_eq!(run(script), expected);
}

/// Every class gets a delegate, `<namespace>:: oo ::delegate`, made before its
/// definition script runs and mixed into it afterwards with its superclasses'
/// delegates as superclasses (`TclOO_Class_Constructor`, `MixinClassDelegates`);
/// `classmethod` puts a method on the delegate and a `myclass` forward on the
/// class; `oo::copy` of a class copies the delegate (`TclOO_Class_Cloned`);
/// `definitionnamespace` and `info class definitionnamespace` choose where
/// `oo::define` and `oo::objdefine` run (`TclOOGetDefineContextNamespace`);
/// `oo::DelegateName` and `oo::UnknownDefinition`.
#[test]
fn class_delegates_classmethod_and_definition_namespaces() {
    let script = r#"oo::class create A
oo::class create B {superclass A}
puts [oo::DelegateName A]|[oo::DelegateName B]
set d [oo::DelegateName B]
puts [info object class $d]|[info class superclasses $d]|[info object mixins B]|[info object mixins A]
puts [info class superclasses [oo::DelegateName A]]
puts [info object namespace $d]
puts [lrange [info class instances oo::class] end-3 end]
puts [catch {oo::DelegateName} m]$m
puts [catch {oo::DelegateName nope} m]$m
puts [catch {oo::DelegateName [B new]} m]$m
oo::define A classmethod cm {x} {return "cm:[self]:$x"}
puts [B cm 1]|[A cm 2]
puts [catch {[B new] cm 3} m]$m
puts [info class methods $d]|[info class methods [oo::DelegateName A]]
oo::class create Z {classmethod zz {} {return zz}}
puts [Z zz]
puts [info object methods Z]|[lsort [info object methods Z -all]]
puts [info class definition [oo::DelegateName Z] zz]
puts [info class forward Z zz]
puts [info class definitionnamespace A]x
namespace eval ::foo {proc hello {} {return hi}}
namespace eval ::bar {proc hey {} {return hey}}
oo::define A definitionnamespace ::foo
puts [info class definitionnamespace A]
puts [info class definitionnamespace A -instance]x
oo::define A definitionnamespace -instance ::bar
puts [info class definitionnamespace A -instance]
puts [info class definitionnamespace oo::class]|[info class definitionnamespace oo::object -instance]
puts [catch {oo::define A2 hello} m]$m
set ai [A new]
puts [catch {oo::objdefine $ai hey} m]$m
puts [catch {oo::objdefine $ai {hey}} m]$m
puts [catch {oo::objdefine $ai {method x {} {}}} m]$m
oo::define A definitionnamespace {}
puts [info class definitionnamespace A]x
puts [catch {oo::define A classmethod} m]$m
puts [catch {oo::define oo::class definitionnamespace ::foo} m]$m
puts [catch {oo::define A definitionnamespace ::nope} m]$m
puts [catch {oo::define A definitionnamespace -bogus ::foo} m]$m
oo::class create Meta {superclass oo::class}
oo::define Meta definitionnamespace ::foo
Meta create K
puts [catch {oo::define K hello} m]$m
puts [catch {oo::define K {hello}} m]$m
puts [catch {oo::define K {method q {} {}}} m]$m
oo::define Z private classmethod pz {} {return pz}
puts [catch {Z pz} m]$m
puts [info class methods Z -private]
oo::copy Z Z2
puts [Z2 zz][oo::DelegateName Z2]
oo::define Z classmethod zz {} {return zz2}
puts [Z zz][Z2 zz]
puts [info object mixins Z2]
puts [catch {::oo::UnknownDefinition} m]$m|$::errorCode
puts [catch {::oo::UnknownDefinition meth} m]$m
puts [catch {oo::define Z {meth y {} {return y}}} m]$m[[Z new] y]
puts [catch {oo::define Z {s}} m]$m
puts [info commands ::oo::DelegateName][info commands ::oo::UnknownDefinition]
"#;
    let expected = r#"::oo::Obj20:: oo ::delegate|::oo::Obj22:: oo ::delegate
::oo::class|::oo::object {::oo::Obj20:: oo ::delegate}|{::oo::Obj22:: oo ::delegate}|{::oo::Obj20:: oo ::delegate}
::oo::object
::oo::Obj23
::A {::oo::Obj20:: oo ::delegate} ::B {::oo::Obj22:: oo ::delegate}
1wrong # args: should be "oo::DelegateName class"
1nope does not refer to an object
1"::oo::Obj24" is not a class
cm:::B:1|cm:::A:2
0cm:::B:3
|cm
zz
|create destroy new zz
{} {return zz}
myclass zz
x
::foo
x
::bar
::oo::define|::oo::objdefine
1A2 does not refer to an object
0hey
0hey
1invalid command name "method"
x
1wrong # args: should be "classmethod name ?args body?"
1may not modify the definition namespace of the root classes
1namespace "::nope" not found
1bad kind "-bogus": must be -class or -instance
0hi
0hi
1invalid command name "method"
1unknown method "pz": must be create, destroy, new or zz
zz
zz::oo::Obj33:: oo ::delegate
zz2zz
{::oo::Obj33:: oo ::delegate}
1bad call of unknown handler|TCL OO BAD_UNKNOWN
1this command may only be called from within the context of an ::oo::define or ::oo::objdefine command
0y
1invalid command name "s"
::oo::DelegateName::oo::UnknownDefinition
"#;
    assert_eq!(run(script), expected);
}

/// `Tcl_CopyObjectInstance` duplicates the private-variable mappings with their
/// full names, so a copy (object or class) keeps the origin's creation id in
/// them.
#[test]
fn oo_copy_keeps_the_origins_private_variable_names() {
    let script = r#"oo::class create A
oo::objdefine A {private variable z; method sz v {set z $v; my varname z}}
A create o
oo::objdefine o {private variable q; method sq v {set q $v; my varname q}}
puts [o sq 1]
oo::copy o o2
puts [o2 sq 2]
puts [info object variables o2 -private]
oo::copy A A2
puts [A2 sz 3]
oo::class create C {private variable x; method sx v {set x $v; my varname x}}
oo::copy C C2
puts [[C2 new] sx 4]
puts [info class variables C2 -private]
"#;
    let expected = r#"::oo::Obj22::22 : q
::oo::Obj23::22 : q
q
::oo::Obj24::20 : z
::oo::Obj30::26 : x
x
"#;
    assert_eq!(run(script), expected);
}

/// The slots are objects (`TclOODefineSlots`): `oo::Slot` with `-append`,
/// `-appendifnew`, `-clear`, `-prepend`, `-remove`, `-set` built on `Get`,
/// `Set` and `Resolve`, and an `unknown` that hands a non-option word to the
/// `--default-operation`; `::oo::define::superclass` and the other standard
/// slots are its instances, so a definition script can ask `info object class
/// superclass` and a script can subclass `oo::Slot`.
#[test]
fn slots_are_objects_of_oo_slot() {
    let script = r#"puts [info object class ::oo::define::superclass]|[info object namespace ::oo::Slot]|[info object namespace ::oo::define::filter]|[info object namespace ::oo::objdefine::variable]
puts [lsort [info class methods oo::Slot -private]]
puts [lsort [info class methods oo::Slot]]
puts [lsort [info object methods ::oo::define::superclass -private]]
puts [lsort [info object methods ::oo::define::filter -private]]
puts [lrange [info class instances oo::Slot] 0 6]
oo::class create A; oo::class create B
oo::class create C {
  superclass A
  puts [info object class superclass]
  superclass -append B
  puts [info class superclasses [self]]
  variable x y
  variable -append z
  filter -set f1 f2
  filter -remove f1
}
puts [info class superclasses C]|[info class variables C]|[info class filters C]
oo::define C superclass -clear
puts [info class superclasses C]
puts [catch {oo::define C superclass -clear x} m]$m
puts [catch {oo::define C superclass -bogus x} m]$m
puts [catch {oo::define C {superclass -bogus x}} m]$m
puts [catch {::oo::define::superclass A} m]$m
puts [catch {oo::define C {superclass Get x}} m]$m
puts [catch {oo::define C {superclass Set}} m]$m
puts [catch {oo::define C {superclass Set a b}} m]$m
puts [catch {oo::define C {mixin -set nope}} m]$m
puts [catch {oo::define C {superclass Resolve}} m]$m
puts [catch {::oo::Slot new} m]$m
oo::define C mixin -append A B
puts [info class mixins C]
oo::define C mixin -remove A
puts [info class mixins C]
oo::define C mixin -prepend A
puts [info class mixins C]
oo::define C mixin -appendifnew B A
puts [info class mixins C]
C create o
oo::objdefine o {mixin A; variable q; filter -append g}
puts [info object mixins o]|[info object variables o]|[info object filters o]
oo::objdefine o mixin
puts [info object mixins o]x
oo::define C {superclass}
puts [info class superclasses C]
oo::define C variable
puts [info class variables C]
oo::class create MySlot {superclass oo::Slot; variable items; method Get {} {return $items}; method Set {l} {set items $l}; method Resolve {x} {string toupper $x}}
MySlot create s
s -set a b; s -append c; s d
puts [s Get]
puts [catch {s destroy} m]$m
puts [catch {oo::define C {private variable p q}} m]$m[info class variables C -private]
catch {A destroy}; oo::class create A
catch {C destroy}; oo::class create C
puts [catch {oo::define C {superclass -clear x}} m]$m
puts [catch {oo::define C {superclass A A}} m]$m|$::errorCode
puts [catch {oo::define C {superclass C}} m]$m
puts [catch {oo::define C {variable a::b}} m]$m
puts [catch {oo::define C {mixin -append nope}} m]$m
puts [catch {oo::define C superclass {a b}} m]$m
puts [catch {oo::objdefine [C new] {superclass A}} m]$m
puts [catch {oo::define oo::object superclass A} m]$m
oo::define C {mixin A; mixin}
puts [info class mixins C]x
oo::define C filter a b a
puts [info class filters C]
oo::define C {self filter x}
puts [info object filters C]
oo::define C {self {variable v w}}
puts [info object variables C]
puts [info object methods oo::define::superclass -all -private]
puts [::oo::Slot create my1]
puts [catch {my1 -set a} m]$m|$::errorCode
"#;
    let expected = r#"::oo::Slot|::oo::Obj3|::oo::Obj4|::oo::Obj10
--default-operation -append -appendifnew -clear -prepend -remove -set Get Resolve Set unknown
-append -appendifnew -clear -prepend -remove -set
--default-operation Get Resolve Set
Get Set
::oo::define::filter ::oo::define::mixin ::oo::define::superclass ::oo::define::variable ::oo::objdefine::filter ::oo::objdefine::mixin ::oo::objdefine::variable
::oo::Slot
::A ::B
::A ::B|x y z|f2
::oo::object
1wrong # args: should be "oo::define C superclass -clear"
1unknown method "-bogus": must be -append, -appendifnew, -clear, -prepend, -remove or -set
1unknown method "-bogus": must be -append, -appendifnew, -clear, -prepend, -remove or -set
1this command may only be called from within the context of an ::oo::define or ::oo::objdefine command
1Get does not refer to an object
1Set does not refer to an object
1Set does not refer to an object
1nope does not refer to an object
1Resolve does not refer to an object
0::oo::Obj26
::A ::B
::B
::A ::B
::A ::B
::A|q|g
x
::oo::object
x y z
A B C D GET
0A B C D GET DESTROY
0p q
1wrong # args: should be "superclass -clear"
1class should only be a direct superclass once|TCL OO REPETITIOUS
1attempt to form circular dependency graph
1invalid declared variable name "a::b": must not contain namespace separators
1nope does not refer to an object
1a b does not refer to an object
1invalid command name "superclass"
1may not modify the superclass of the root object
x
a b a
x
v w
--default-operation -append -appendifnew -clear -prepend -remove -set <cloned> Get Resolve Set destroy eval unknown variable varname
::my1
1unimplemented|TCL OO ABSTRACT_SLOT
"#;
    assert_eq!(run(script), expected);
}

/// Properties (`generic/tclOOProp.c`): `oo::configurable` mixes
/// `oo::configuresupport::configurable` into its classes, whose definitions run
/// in `oo::configuresupport::configurableclass` where `property` declares
/// `-name` readable and/or writable with standard or scripted `<ReadProp-…>` and
/// `<WriteProp-…>` methods; `configure` reads and writes them by unique prefix;
/// `info class properties` and `info object properties`; the property slots;
/// and `info commands ::oo::*` is tclsh's fourteen names.
#[test]
fn properties_and_oo_configurable() {
    let script = r#"oo::configurable create P {
  property x y
  property ro -kind readable
  property wo -kind writable
  property c -get {return computed} -set {set ::seen $value}
  variable x y ro wo
  constructor {args} {set x 1; set y 2; set ro r; my configure {*}$args}
}
puts [info class properties P]|[info class properties P -writable]|[info class properties P -all]
set p [P new -x 10]
puts [$p configure]
puts [$p configure -x]
$p configure -y 5 -c 7
puts [$p configure -y]|$::seen
puts [catch {$p configure -ro 1} m]$m|$::errorCode
puts [catch {$p configure -wo} m]$m
puts [catch {$p configure -nope} m]$m|$::errorCode
puts [catch {$p configure -x} m]$m
puts [catch {$p configure -x 1 -y} m]$m
puts [lsort [info class methods P -private]]
puts [info class methodtype P <ReadProp-x>]|[info class methodtype P <WriteProp-c>]
puts [info class definition P <ReadProp-c>]
puts [info class mixins P]|[info object class P]|[info class superclasses oo::configurable]
puts [info class methods oo::configuresupport::configurable]
puts [info class definitionnamespace oo::configurable]|[info class definitionnamespace oo::configuresupport::configurable -instance]
oo::objdefine $p property z
puts [info object properties $p]|[info object properties $p -all]|[info object properties $p -all -writable]
puts [catch {$p configure -z} m]$m
oo::configurable create Q {superclass P; property q}
puts [info class properties Q -all]
puts [lsort [info commands ::oo::configuresupport::*]]
puts [catch {oo::define P property -bad} m]$m|$::errorCode
puts [catch {oo::define P property a::b} m]$m
puts [catch {oo::define P property {a b}} m]$m
puts [catch {oo::define P property a(b)} m]$m
puts [catch {oo::define P property x -kind} m]$m|$::errorCode
puts [catch {oo::define P property x -get} m]$m
puts [catch {oo::define P property x -kind bad} m]$m
puts [catch {oo::define P property x -foo y} m]$m
oo::define P {properties zz}
puts [info class properties P]
puts [catch {oo::define P {meth m {} {}}} m]$m
oo::define P {::oo::configuresupport::readableproperties -set -x}
puts [info class properties P]
puts [catch {oo::class create N {property n}} m]$m
puts [lsort [info commands ::oo::*]]
puts [info class instances oo::class]
puts [lrange [info class instances oo::Slot] 7 end]
puts [[oo::class create Fresh] new]
oo::configurable create Cv {property v; variable v}
set cv [Cv new]
puts [catch {$cv configure -v} m]$m|$::errorCode
$cv configure -v 3
puts [$cv configure]
oo::define Cv {property v -kind readable}
puts [info class properties Cv -writable]x[info class properties Cv]
puts [lsort [info class methods Cv -private]]
"#;
    let expected = r#"-c -ro -x -y|-c -wo -x -y|-c -ro -x -y
-c computed -ro r -x 10 -y 2
10
5|7
1property "-ro" is read only|TCL LOOKUP INDEX property -ro
1property "-wo" is write only
1bad property "-nope": must be -c, -ro, -x, or -y|TCL LOOKUP INDEX property -nope
010
1wrong # args: should be "::oo::Obj22 configure ?-option value ...?"
<ReadProp-c> <ReadProp-ro> <ReadProp-x> <ReadProp-y> <WriteProp-c> <WriteProp-wo> <WriteProp-x> <WriteProp-y>
PropertyGetter|method
{} {return computed}
::oo::configuresupport::configurable|::oo::configurable|::oo::class
configure
::oo::configuresupport::configurableclass|::oo::configuresupport::configurableobject
-z|-c -ro -x -y -z|-c -wo -x -y -z
1can't read "z": no such variable
-c -q -ro -x -y
::oo::configuresupport::configurable ::oo::configuresupport::objreadableproperties ::oo::configuresupport::objwritableproperties ::oo::configuresupport::readableproperties ::oo::configuresupport::writableproperties
1bad property name "-bad": must not begin with -|TCL OO PROPERTY_FORMAT
1bad property name "a::b": must not contain namespace separators
1bad property name "a b": must be a simple word
1bad property name "a(b)": must not contain parentheses
1missing kind value to go with -kind option|TCL WRONGARGS
1missing body to go with -get option
1bad kind "bad": must be readable, readwrite, or writable
1bad option "-foo": must be -get, -kind, or -set
-c -ro -x -y -zz
1invalid command name "meth"
-x
1invalid command name "property"
::oo::DelegateName ::oo::InfoClass ::oo::InfoObject ::oo::Obj22 ::oo::SingletonInstance ::oo::Slot ::oo::UnknownDefinition ::oo::abstract ::oo::class ::oo::configurable ::oo::copy ::oo::define ::oo::objdefine ::oo::object ::oo::singleton
::oo::object ::oo::class ::oo::Slot ::oo::singleton ::oo::SingletonInstance ::oo::abstract ::oo::configuresupport::configurable ::oo::configurable {::oo::Obj20:: oo ::delegate} {::oo::Obj23:: oo ::delegate}
::oo::configuresupport::readableproperties ::oo::configuresupport::writableproperties ::oo::configuresupport::objreadableproperties ::oo::configuresupport::objwritableproperties
::oo::Obj29
1can't read "v": no such variable|TCL READ VARNAME
-v 3
x-v
<ReadProp-v> <WriteProp-v>
"#;
    assert_eq!(run(script), expected);
}
