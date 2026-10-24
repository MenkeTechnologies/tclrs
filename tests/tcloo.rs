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
