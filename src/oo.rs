//! TclOO — `oo::object`, `oo::class`, `oo::singleton`, `oo::abstract`,
//! `oo::define`, `oo::objdefine`, `oo::copy`, `self`, `my`, `next`, `nextto`
//! and `info object` / `info class`.
//!
//! A port of the object system in `generic/tclOO*.c` (Tcl 9.0.4), measured
//! against tclsh 9.1.
//!
//! # What an object is here
//!
//! An object is a record in [`Oo`], which the interpreter holds: its command
//! name, its namespace (`::oo::Obj<n>`, numbered as `AllocObject` numbers
//! them), the class it is an instance of, its own methods, mixins, filters and
//! declared variables, and — for a class — the class half (`Class` in
//! `tclOOInt.h`). Every object's command and namespace are recorded in the
//! namespace registry too, so `info commands`, `namespace exists` and
//! `namespace which` see them.
//!
//! A call to an object reaches this module from the run-time call op
//! ([`crate::procs::call_op`]): the compiler cannot know an object's name, so
//! it lowers the call to the run-time lookup, and the lookup asks
//! [`try_dispatch`] before it reports `invalid command name`.
//!
//! # Call chains
//!
//! [`Oo::chain_for`] is `TclOOGetCallContext` without the caches: filters
//! first, then the object's mixins, the object's own methods, the class's
//! mixins and the class hierarchy, with a method that appears twice kept at its
//! *later* position (`AddMethodToCallChain`). `next` walks the same chain.
//!
//! # Methods are procedures
//!
//! A procedure-like method's body is compiled — once, on its first call — as
//! a procedure of the namespace [`METHOD_NS`], which no script can name. That
//! namespace stands for "the namespace of the object whose method is running":
//! a variable the body reaches through `variable`, through `my variable` or
//! through the class's `variable` declaration is compiled as a variable of
//! [`METHOD_NS`], and the interpreter maps that name to the running object's
//! namespace each time a method's chunk is entered and left (see
//! `State::alias_of` in `crate::runtime`). So one compiled body serves every
//! instance, and each call reads and writes its own object's variables.
//!
//! The method context `self`, `my` and `next` answer from is a stack of
//! [`Frame`]s the interpreter holds, pushed around each method invocation and
//! around each definition script.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use fusevm::{Value, VM};

use crate::cmd_namespace::{parent_of, qualifiers, resolve, store_key, tail, Registry};
use crate::procs::{RuntimeProc, Signature};
use crate::runtime::{to_tcl_string, Shared, TclError};

/// The namespace a method body is compiled in, standing for the running
/// object's namespace. The control character keeps any script from naming it.
pub(crate) const METHOD_NS: &str = "::oo::\u{2}method";
/// The variable-map prefix of [`METHOD_NS`]'s variables.
pub(crate) const METHOD_NS_KEY: &str = "oo::\u{2}method::";
/// Separates the namespace a run-time call was compiled in from the command
/// name, in the name operand of a run-time call written inside a namespace.
pub(crate) const NS_MARK: char = '\u{1}';
/// The command `namespace current` lowers to inside a method body.
pub(crate) const CURRENT_NS_CMD: &str = "\u{0}oo::current";
/// The hidden procedure-name prefix of a compiled method body; what follows it
/// is the context line `errorInfo` reports, `class "::C" method "m"`.
pub(crate) const METHOD_PROC_PREFIX: &str = "\u{0}oo:";
/// The [`METHOD_NS`] sub-namespace a method body's `classvariable` names are
/// compiled in: it stands for the namespace of the class that declared the
/// running method.
pub(crate) const CLASS_VAR_MARK: &str = "\u{3}class::";
/// The check a compiled `classvariable` runs, raising what
/// `TclOOClassVariableObjCmd` raises before it links anything.
pub(crate) const CLASSVAR_CHECK_CMD: &str = "\u{0}oo::classvariable";
/// The declaration a method body opens with for a private variable (TIP 500):
/// `PRIVATE_VAR_CMD fullName name` makes the local `name` the object variable
/// `fullName`.
pub(crate) const PRIVATE_VAR_CMD: &str = "\u{0}oo::privatevariable";
/// The definition namespaces of `oo::configurable`'s classes and their
/// instances, which hold `property` and `properties` and reach the rest of
/// `oo::define` / `oo::objdefine` through their namespace path.
pub(crate) const CFG_CLASS_NS: &str = "::oo::configuresupport::configurableclass";
pub(crate) const CFG_OBJECT_NS: &str = "::oo::configuresupport::configurableobject";

/// `TCLOO_VERSION` and `TCLOO_PATCHLEVEL` (`generic/tclOO.h`).
pub(crate) const TCLOO_VERSION: &str = "1.3";
pub(crate) const TCLOO_PATCHLEVEL: &str = "1.3.1";

pub(crate) type Id = usize;

/// The classes `MakeAdditionalClasses` makes, by their place in [`Oo::objects`].
const SINGLETON: Id = 2;
const SINGLETON_INSTANCE: Id = 3;
const ABSTRACT: Id = 4;

// Method flags (`enum TclOOCallChainFlags`, `tclOOInt.h`).
const PUBLIC_METHOD: u32 = 0x01;
const CONSTRUCTOR: u32 = 0x08;
const DESTRUCTOR: u32 = 0x10;
const TRUE_PRIVATE_METHOD: u32 = 0x20;
const OO_UNKNOWN_METHOD: u32 = 0x04;
const FORCE_UNKNOWN: u32 = 0x10000;
const FILTER_HANDLING: u32 = 0x2000;
// Chain-building flags (`enum CallChainFlags`, `tclOOCall.c`).
const DEFINITE_PROTECTED: u32 = 0x100000;
const DEFINITE_PUBLIC: u32 = 0x200000;
const KNOWN_STATE: u32 = DEFINITE_PROTECTED | DEFINITE_PUBLIC;
const SPECIAL: u32 = CONSTRUCTOR | DESTRUCTOR | FORCE_UNKNOWN;
const BUILDING_MIXINS: u32 = 0x400000;
const TRAVERSED_MIXIN: u32 = 0x800000;
const OBJECT_MIXIN: u32 = 0x1000000;

/// `MIXIN_CONSISTENT`: either we are looking for mixin methods and have passed
/// through a mixin, or we are looking for neither.
fn mixin_consistent(flags: u32) -> bool {
    flags & OBJECT_MIXIN != 0 || ((flags & BUILDING_MIXINS == 0) == (flags & TRAVERSED_MIXIN == 0))
}

/// `PUBLIC_PATTERN`: a method whose name starts with a lower-case letter is
/// exported by default.
fn public_by_default(name: &str) -> u32 {
    if name.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
        PUBLIC_METHOD
    } else {
        0
    }
}

// ── the data model ───────────────────────────────────────────────────────

/// A method's implementation.
pub(crate) enum Imp {
    /// A procedure-like method (`TclOONewProcMethod`).
    Proc(Box<ProcImp>),
    /// A forwarded method: the prefix the call's arguments are appended to.
    Forward(Vec<String>),
    /// One of the methods `oo::object` and `oo::class` are born with.
    Builtin(Builtin),
    /// A property's standard getter or setter (`Configurable_Getter`,
    /// `Configurable_Setter`): the variable of the object it reads or writes.
    Property { var: String, setter: bool },
}

/// A procedure-like method: its argument list, its body, and the procedure the
/// body compiled to, once it has been called.
pub(crate) struct ProcImp {
    params: String,
    body: String,
    sig: Signature,
    /// The compiled procedure, with the declared variables and the context line
    /// it was compiled for — a change to either recompiles it.
    compiled: Mutex<Option<(Vec<DeclaredVar>, String, RuntimeProc)>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Builtin {
    Destroy,
    Eval,
    Unknown,
    Variable,
    VarName,
    Cloned,
    Create,
    New,
    CreateNs,
    ClassCtor,
    /// `TclOO_Singleton_New`.
    SingletonNew,
    /// `TclOO_SingletonInstance_Destroy`.
    SingletonDestroy,
    /// `TclOO_SingletonInstance_Cloned`.
    SingletonCloned,
    /// `TclOO_Class_Cloned`.
    ClassCloned,
    /// `oo::Slot`'s `Get` and `Set`: `Slot_Unimplemented`.
    SlotUnimplemented,
    /// `oo::Slot`'s `Resolve`: `Slot_Resolve`.
    SlotResolve,
    /// `oo::Slot`'s `-append`, `-appendifnew`, `-clear`, `-prepend`, `-remove`
    /// and `-set`.
    SlotOp(&'static str),
    /// `oo::Slot`'s `unknown`: `Slot_Unknown`.
    SlotUnknown,
    /// A standard slot's `Get`: which slot, and whether it is `oo::define`'s.
    SlotGet(Slot, bool),
    /// A standard slot's `Set`.
    SlotSet(Slot, bool),
    /// `Slot_ResolveClass`, the `Resolve` of the `mixin` and `superclass` slots.
    SlotResolveClass,
    /// `oo::configuresupport::configurable`'s `configure`:
    /// `TclOO_Configurable_Configure`.
    Configure,
    /// `oo::configurable`'s constructor: `TclOO_Configurable_Constructor`.
    ConfigurableCtor,
}

/// A class's or an object's declared properties (`PropertyStorage`): the
/// readable and the writable ones, each a set kept in declaration order, with
/// the names hyphenated as `configure` takes them.
#[derive(Clone, Default)]
struct Props {
    readable: Vec<String>,
    writable: Vec<String>,
}

/// One entry of a method table: the export flags and, unless the record only
/// carries flags for a method defined elsewhere, the implementation.
#[derive(Clone)]
struct Method {
    flags: u32,
    imp: Option<Arc<Imp>>,
}

#[derive(Default)]
struct ClassData {
    superclasses: Vec<Id>,
    subclasses: Vec<Id>,
    instances: Vec<Id>,
    mixins: Vec<Id>,
    /// The classes and objects that mix this class in.
    mixin_users: Vec<Id>,
    filters: Vec<String>,
    variables: Vec<String>,
    /// The variables declared under `private` (TIP 500).
    private_vars: Vec<String>,
    /// The creation id the private variables are named with
    /// (`PrivateVariableMapping`'s `fullNameObj`): the declarer's own when
    /// they were declared, the origin's in a copy.
    private_epoch: u64,
    methods: BTreeMap<String, Method>,
    constructor: Option<Method>,
    destructor: Option<Method>,
    /// `clsDefinitionNs`: where `oo::define` runs for a class whose class
    /// this is (`definitionnamespace -class`).
    cls_def_ns: Option<String>,
    /// `objDefinitionNs`: where `oo::objdefine` runs for an instance
    /// (`definitionnamespace -instance`).
    obj_def_ns: Option<String>,
    /// The properties the class declares (`PropertyStorage`).
    properties: Props,
}

struct Object {
    /// The object's command, fully qualified.
    command: String,
    /// The object's namespace, fully qualified.
    ns: String,
    /// The class this is an instance of (`selfCls`).
    class: Id,
    creation_id: u64,
    methods: BTreeMap<String, Method>,
    mixins: Vec<Id>,
    filters: Vec<String>,
    variables: Vec<String>,
    /// The variables declared under `private` (TIP 500).
    private_vars: Vec<String>,
    /// The creation id the private variables are named with
    /// (`PrivateVariableMapping`'s `fullNameObj`): the declarer's own when
    /// they were declared, the origin's in a copy.
    private_epoch: u64,
    /// The properties the object declares of its own (`PropertyStorage`).
    properties: Props,
    class_data: Option<ClassData>,
    /// The destructor has been called (`DESTRUCTOR_CALLED`).
    destructing: bool,
    /// A filter of this object is running (`FILTER_HANDLING`).
    filter_handling: bool,
}

/// Who declared a method in a chain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Declarer {
    Class(Id),
    Object(Id),
}

/// One method implementation in a call chain (`MInvoke`).
#[derive(Clone)]
pub(crate) struct Call {
    imp: Arc<Imp>,
    name: String,
    declarer: Declarer,
    filter: bool,
    filter_declarer: Option<Id>,
    private: bool,
}

/// A call chain (`CallChain`).
pub(crate) struct Chain {
    calls: Vec<Call>,
    flags: u32,
}

/// What `self`, `my` and `next` answer from: the innermost method invocation or
/// definition script.
#[derive(Clone)]
pub(crate) enum Frame {
    Method {
        obj: Id,
        chain: Arc<Chain>,
        index: usize,
    },
    Define {
        obj: Id,
        /// `oo::define` (the class's definition) rather than `oo::objdefine`.
        class: bool,
        private: bool,
        /// The namespace the definition was invoked from, which a class name
        /// inside it is resolved against (`GetClassInOuterContext`).
        outer: String,
        /// The namespace the definition runs in (`TclOOGetDefineContextNamespace`):
        /// `::oo::define`, `::oo::objdefine` or a definition namespace.
        space: String,
    },
}

/// The object system of one interpreter (`Foundation`).
pub(crate) struct Oo {
    objects: Vec<Option<Object>>,
    by_command: std::collections::HashMap<String, Id>,
    by_ns: std::collections::HashMap<String, Id>,
    ns_count: u64,
    pub(crate) stack: Vec<Frame>,
    object_cls: Id,
    class_cls: Id,
    /// The command and namespace names of deleted objects, by id.
    graveyard: std::collections::HashMap<Id, (String, String)>,
    /// The commands `link` made, by key: the object whose `my` each calls and
    /// the method it names (`TclAliasCreate` onto the object's `my`).
    links: BTreeMap<String, (Id, String)>,
    /// The objects whose deletion is under way (`OBJECT_DESTRUCTING`): a
    /// descendant that reaches back to one of them leaves it alone.
    deleting: HashSet<Id>,
}

impl Default for Oo {
    fn default() -> Self {
        Oo::new()
    }
}

fn builtin(flags: u32, b: Builtin) -> Method {
    Method {
        flags,
        imp: Some(Arc::new(Imp::Builtin(b))),
    }
}

impl Oo {
    /// `InitFoundation`: `oo::object` and `oo::class`, spliced together by hand,
    /// with the methods `TclOODefineBasicMethods` gives them.
    fn new() -> Oo {
        let mut object_methods = BTreeMap::new();
        object_methods.insert(
            "destroy".to_string(),
            builtin(PUBLIC_METHOD, Builtin::Destroy),
        );
        object_methods.insert("eval".to_string(), builtin(0, Builtin::Eval));
        object_methods.insert("unknown".to_string(), builtin(0, Builtin::Unknown));
        object_methods.insert("variable".to_string(), builtin(0, Builtin::Variable));
        object_methods.insert("varname".to_string(), builtin(0, Builtin::VarName));
        object_methods.insert("<cloned>".to_string(), builtin(0, Builtin::Cloned));
        let mut class_methods = BTreeMap::new();
        class_methods.insert(
            "create".to_string(),
            builtin(PUBLIC_METHOD, Builtin::Create),
        );
        class_methods.insert("new".to_string(), builtin(PUBLIC_METHOD, Builtin::New));
        class_methods.insert(
            "createWithNamespace".to_string(),
            builtin(0, Builtin::CreateNs),
        );
        class_methods.insert("<cloned>".to_string(), builtin(0, Builtin::ClassCloned));
        let object = Object {
            command: "::oo::object".to_string(),
            ns: "::oo::Obj1".to_string(),
            class: 1,
            creation_id: 1,
            methods: BTreeMap::new(),
            mixins: Vec::new(),
            filters: Vec::new(),
            variables: Vec::new(),
            private_vars: Vec::new(),
            private_epoch: 0,
            properties: Props::default(),
            class_data: Some(ClassData {
                subclasses: vec![1],
                methods: object_methods,
                obj_def_ns: Some("::oo::objdefine".to_string()),
                ..ClassData::default()
            }),
            destructing: false,
            filter_handling: false,
        };
        // `new` is not exported on `oo::class` itself: a class must be named.
        let mut class_own = BTreeMap::new();
        class_own.insert(
            "new".to_string(),
            Method {
                flags: 0,
                imp: None,
            },
        );
        let class = Object {
            command: "::oo::class".to_string(),
            ns: "::oo::Obj2".to_string(),
            class: 1,
            creation_id: 2,
            methods: class_own,
            mixins: Vec::new(),
            filters: Vec::new(),
            variables: Vec::new(),
            private_vars: Vec::new(),
            private_epoch: 0,
            properties: Props::default(),
            class_data: Some(ClassData {
                superclasses: vec![0],
                instances: vec![0, 1],
                methods: class_methods,
                constructor: Some(builtin(PUBLIC_METHOD, Builtin::ClassCtor)),
                cls_def_ns: Some("::oo::define".to_string()),
                ..ClassData::default()
            }),
            destructing: false,
            filter_handling: false,
        };
        let mut oo = Oo {
            objects: vec![Some(object), Some(class)],
            by_command: Default::default(),
            by_ns: Default::default(),
            // The objects tclsh 9.1 makes for itself before a script runs
            // (`oo::Slot`, the slots, `oo::singleton` and the rest) use up the
            // numbers to 19, so a script's first object is `::oo::Obj20`.
            ns_count: 19,
            stack: Vec::new(),
            object_cls: 0,
            class_cls: 1,
            graveyard: Default::default(),
            links: BTreeMap::new(),
            deleting: HashSet::new(),
        };
        oo.make_additional_classes();
        oo.make_slots();
        oo.make_configurable();
        for id in [0, 1, SINGLETON, SINGLETON_INSTANCE, ABSTRACT] {
            let (cmd, ns) = {
                let o = oo.obj(id);
                (store_key(&o.command).to_string(), o.ns.clone())
            };
            oo.by_command.insert(cmd, id);
            oo.by_ns.insert(ns, id);
        }
        oo
    }

    /// The part of `MakeAdditionalClasses` (`generic/tclOO.c`) this port has:
    /// `oo::singleton`, `oo::SingletonInstance` and `oo::abstract`, each an
    /// instance of `oo::class` under the namespace and creation number tclsh
    /// gives it, with `TclOODefineBasicMethods`' methods and
    /// `TclOOUnexportMethods`' records. `MarkAsMetaclass` makes the two
    /// metaclasses subclasses of `oo::class`.
    fn make_additional_classes(&mut self) {
        let hidden = || Method {
            flags: 0,
            imp: None,
        };
        let mut singleton = BTreeMap::new();
        singleton.insert(
            "new".to_string(),
            builtin(PUBLIC_METHOD, Builtin::SingletonNew),
        );
        singleton.insert("create".to_string(), hidden());
        singleton.insert("createWithNamespace".to_string(), hidden());
        let mut instance = BTreeMap::new();
        instance.insert(
            "destroy".to_string(),
            builtin(PUBLIC_METHOD, Builtin::SingletonDestroy),
        );
        instance.insert("<cloned>".to_string(), builtin(0, Builtin::SingletonCloned));
        let mut abstract_ = BTreeMap::new();
        for name in ["create", "createWithNamespace", "new"] {
            abstract_.insert(name.to_string(), hidden());
        }
        for (id, command, number, superclass, methods) in [
            (SINGLETON, "::oo::singleton", 15, self.class_cls, singleton),
            (
                SINGLETON_INSTANCE,
                "::oo::SingletonInstance",
                16,
                self.object_cls,
                instance,
            ),
            (ABSTRACT, "::oo::abstract", 17, self.class_cls, abstract_),
        ] {
            debug_assert_eq!(self.objects.len(), id);
            self.objects.push(Some(Object {
                command: command.to_string(),
                ns: format!("::oo::Obj{number}"),
                class: self.class_cls,
                creation_id: number,
                methods: BTreeMap::new(),
                mixins: Vec::new(),
                filters: Vec::new(),
                variables: Vec::new(),
                private_vars: Vec::new(),
                private_epoch: 0,
                properties: Props::default(),
                class_data: Some(ClassData {
                    superclasses: vec![superclass],
                    methods,
                    ..ClassData::default()
                }),
                destructing: false,
                filter_handling: false,
            }));
            self.class_mut(superclass).subclasses.push(id);
            let class_cls = self.class_cls;
            self.class_mut(class_cls).instances.push(id);
        }
    }

    /// `TclOODefineSlots`: the class `oo::Slot` (`::oo::Obj3`) with its
    /// `slotMethods`, a `--default-operation` forward to `-append` and an
    /// unexported `destroy`; then the standard slots, `::oo::Obj4` onwards,
    /// each with its own `Get`, `Set` and — for the class-valued ones —
    /// `Resolve`, and a `--default-operation` forward to `-set` where the
    /// slot's default is not `-append`.
    fn make_slots(&mut self) {
        let mut methods = BTreeMap::new();
        for (name, flags, b) in [
            ("Get", 0, Builtin::SlotUnimplemented),
            ("Resolve", 0, Builtin::SlotResolve),
            ("Set", 0, Builtin::SlotUnimplemented),
            ("-append", PUBLIC_METHOD, Builtin::SlotOp("-append")),
            (
                "-appendifnew",
                PUBLIC_METHOD,
                Builtin::SlotOp("-appendifnew"),
            ),
            ("-clear", PUBLIC_METHOD, Builtin::SlotOp("-clear")),
            ("-prepend", PUBLIC_METHOD, Builtin::SlotOp("-prepend")),
            ("-remove", PUBLIC_METHOD, Builtin::SlotOp("-remove")),
            ("-set", PUBLIC_METHOD, Builtin::SlotOp("-set")),
            ("unknown", 0, Builtin::SlotUnknown),
        ] {
            methods.insert(name.to_string(), builtin(flags, b));
        }
        let forward = |op: &str| Method {
            flags: 0,
            imp: Some(Arc::new(Imp::Forward(vec![
                "my".to_string(),
                op.to_string(),
            ]))),
        };
        methods.insert("--default-operation".to_string(), forward("-append"));
        methods.insert(
            "destroy".to_string(),
            Method {
                flags: 0,
                imp: None,
            },
        );
        let slot_cls = self.push_builtin_object("::oo::Slot", 3, Some((self.object_cls, methods)));
        // `TclOODefineSlots` runs before `MakeAdditionalClasses`, so `oo::Slot`
        // comes before `oo::singleton` in the lists it joins.
        let (object_cls, class_cls) = (self.object_cls, self.class_cls);
        self.class_mut(class_cls)
            .instances
            .retain(|&x| x != slot_cls);
        self.class_mut(class_cls).instances.insert(2, slot_cls);
        self.class_mut(object_cls)
            .subclasses
            .retain(|&x| x != slot_cls);
        self.class_mut(object_cls).subclasses.insert(1, slot_cls);
        let slots = [
            ("::oo::define::filter", Slot::Filter, true),
            ("::oo::define::mixin", Slot::Mixin, true),
            ("::oo::define::superclass", Slot::Superclass, true),
            ("::oo::define::variable", Slot::Variable, true),
            ("::oo::objdefine::filter", Slot::Filter, false),
            ("::oo::objdefine::mixin", Slot::Mixin, false),
            ("::oo::objdefine::variable", Slot::Variable, false),
            (
                "::oo::configuresupport::readableproperties",
                Slot::ReadableProps,
                true,
            ),
            (
                "::oo::configuresupport::writableproperties",
                Slot::WritableProps,
                true,
            ),
            (
                "::oo::configuresupport::objreadableproperties",
                Slot::ReadableProps,
                false,
            ),
            (
                "::oo::configuresupport::objwritableproperties",
                Slot::WritableProps,
                false,
            ),
        ];
        for (i, (command, which, class)) in slots.into_iter().enumerate() {
            let id = self.push_builtin_object(command, 4 + i as u64, None);
            self.obj_mut(id).class = slot_cls;
            let own = &mut self.obj_mut(id).methods;
            own.insert(
                "Get".to_string(),
                builtin(0, Builtin::SlotGet(which, class)),
            );
            own.insert(
                "Set".to_string(),
                builtin(0, Builtin::SlotSet(which, class)),
            );
            if matches!(which, Slot::Mixin | Slot::Superclass) {
                own.insert("Resolve".to_string(), builtin(0, Builtin::SlotResolveClass));
                own.insert("--default-operation".to_string(), forward("-set"));
            }
            let class_cls = self.class_cls;
            self.class_mut(class_cls).instances.retain(|&x| x != id);
            self.class_mut(slot_cls).instances.push(id);
        }
    }

    /// The rest of `MakeAdditionalClasses`: `oo::configuresupport::configurable`
    /// (`::oo::Obj18`), whose `configure` the classes of `oo::configurable`
    /// mix in, and the metaclass `oo::configurable` (`::oo::Obj19`) itself,
    /// with the definition namespaces that hold `property`.
    fn make_configurable(&mut self) {
        let mut methods = BTreeMap::new();
        methods.insert(
            "configure".to_string(),
            builtin(PUBLIC_METHOD, Builtin::Configure),
        );
        let support = self.push_builtin_object(
            "::oo::configuresupport::configurable",
            18,
            Some((self.object_cls, methods)),
        );
        let c = self.class_mut(support);
        c.cls_def_ns = Some(CFG_CLASS_NS.to_string());
        c.obj_def_ns = Some(CFG_OBJECT_NS.to_string());
        let class_cls = self.class_cls;
        let configurable =
            self.push_builtin_object("::oo::configurable", 19, Some((class_cls, BTreeMap::new())));
        let c = self.class_mut(configurable);
        c.constructor = Some(builtin(PUBLIC_METHOD, Builtin::ConfigurableCtor));
        c.cls_def_ns = Some(CFG_CLASS_NS.to_string());
    }

    /// An object `InitFoundation` makes before any script runs: an instance of
    /// `oo::class` named `command` with namespace and creation id `number`,
    /// and — when `class` gives its superclass and methods — a class.
    fn push_builtin_object(
        &mut self,
        command: &str,
        number: u64,
        class: Option<(Id, BTreeMap<String, Method>)>,
    ) -> Id {
        let id = self.objects.len();
        let is_class = class.is_some();
        self.objects.push(Some(Object {
            command: command.to_string(),
            ns: format!("::oo::Obj{number}"),
            class: self.class_cls,
            creation_id: number,
            methods: BTreeMap::new(),
            mixins: Vec::new(),
            filters: Vec::new(),
            variables: Vec::new(),
            private_vars: Vec::new(),
            private_epoch: 0,
            properties: Props::default(),
            class_data: class.map(|(superclass, methods)| ClassData {
                superclasses: vec![superclass],
                methods,
                ..ClassData::default()
            }),
            destructing: false,
            filter_handling: false,
        }));
        if let (true, Some(Some(o))) = (is_class, self.objects.get(id)) {
            let superclass = o.class_data.as_ref().expect("a class").superclasses[0];
            self.class_mut(superclass).subclasses.push(id);
        }
        let class_cls = self.class_cls;
        self.class_mut(class_cls).instances.push(id);
        self.by_command.insert(store_key(command).to_string(), id);
        self.by_ns.insert(format!("::oo::Obj{number}"), id);
        id
    }

    fn obj(&self, id: Id) -> &Object {
        self.objects[id].as_ref().expect("a live object")
    }

    fn obj_mut(&mut self, id: Id) -> &mut Object {
        self.objects[id].as_mut().expect("a live object")
    }

    fn alive(&self, id: Id) -> bool {
        self.objects.get(id).is_some_and(Option::is_some)
    }

    fn class(&self, id: Id) -> &ClassData {
        self.obj(id).class_data.as_ref().expect("a class")
    }

    fn class_mut(&mut self, id: Id) -> &mut ClassData {
        self.obj_mut(id).class_data.as_mut().expect("a class")
    }

    fn is_class(&self, id: Id) -> bool {
        self.obj(id).class_data.is_some()
    }

    /// The object's command name, as `TclOOObjectName` reports it — the
    /// name it had when it was deleted, for an object whose method is still
    /// running after its deletion.
    fn name(&self, id: Id) -> String {
        match &self.objects[id] {
            Some(o) => o.command.clone(),
            None => self
                .graveyard
                .get(&id)
                .map(|(c, _)| c.clone())
                .unwrap_or_default(),
        }
    }

    /// The object's namespace, likewise.
    fn ns_of(&self, id: Id) -> String {
        match &self.objects[id] {
            Some(o) => o.ns.clone(),
            None => self
                .graveyard
                .get(&id)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
        }
    }

    /// The object of the innermost running method.
    fn current_method(&self) -> Option<(Id, &Arc<Chain>, usize)> {
        self.stack.iter().rev().find_map(|f| match f {
            Frame::Method { obj, chain, index } => Some((*obj, chain, *index)),
            Frame::Define { .. } => None,
        })
    }

    /// The variable-map key of the running object's variable `rest`, for a
    /// [`METHOD_NS`] variable a method chunk names. With no method running —
    /// or the object gone — the name goes nowhere: a key beginning with NUL,
    /// which the interpreter never stores.
    pub(crate) fn method_var_key(&self, rest: &str) -> String {
        if let Some(name) = rest.strip_prefix(CLASS_VAR_MARK) {
            return match self.current_method() {
                Some((_, chain, index)) => match chain.calls[index].declarer {
                    Declarer::Class(c) if self.alive(c) => {
                        format!("{}::{name}", store_key(&self.obj(c).ns))
                    }
                    _ => format!("\u{0}oo-gone::{name}"),
                },
                None => format!("\u{0}oo-gone::{name}"),
            };
        }
        match self.current_method() {
            Some((obj, ..)) if self.alive(obj) => {
                format!("{}::{rest}", store_key(&self.obj(obj).ns))
            }
            _ => format!("\u{0}oo-gone::{rest}"),
        }
    }

    /// `TclOOGetDefineContextNamespace`: the namespace `oo::define` (when
    /// `for_class`) or `oo::objdefine` runs a definition of `obj` in — the
    /// first definition namespace along `obj`'s class and mixins, with their
    /// superclasses and class mixins (`AddSimpleDefineNamespaces`, mixins
    /// first), that names an existing namespace.
    fn define_space(&self, reg: &Registry, obj: Id, for_class: bool) -> Option<String> {
        let mut chain: Vec<(Id, String)> = Vec::new();
        for flags in [BUILDING_MIXINS, 0] {
            for &m in &self.obj(obj).mixins {
                self.add_define_namespaces(m, &mut chain, for_class, flags | TRAVERSED_MIXIN);
            }
            self.add_define_namespaces(self.obj(obj).class, &mut chain, for_class, flags);
        }
        chain
            .into_iter()
            .map(|(_, ns)| ns)
            .find(|ns| reg.exists(ns))
    }

    /// `AddSimpleClassDefineNamespaces` and `AddDefinitionNamespaceToChain`.
    fn add_define_namespaces(
        &self,
        cls: Id,
        chain: &mut Vec<(Id, String)>,
        for_class: bool,
        flags: u32,
    ) {
        if !self.alive(cls) || !self.is_class(cls) {
            return;
        }
        let c = self.class(cls);
        for &m in &c.mixins {
            self.add_define_namespaces(m, chain, for_class, flags | TRAVERSED_MIXIN);
        }
        let ns = if for_class {
            &c.cls_def_ns
        } else {
            &c.obj_def_ns
        };
        if let (Some(ns), true) = (ns, mixin_consistent(flags)) {
            match chain.iter().position(|(d, _)| *d == cls) {
                // The entry moves as late as it can, exactly as the C shifts
                // the entries after it down one and writes it at its old place.
                Some(i) if i + 1 < chain.len() => {
                    let len = chain.len();
                    for j in i..len - 1 {
                        chain[j] = chain[j + 1].clone();
                    }
                    chain[i] = (cls, ns.clone());
                }
                Some(_) => {}
                None => chain.push((cls, ns.clone())),
            }
        }
        for &s in &c.superclasses {
            self.add_define_namespaces(s, chain, for_class, flags);
        }
    }

    /// The class delegate of `cls` (`GetClassDelegate`): the class named
    /// `<namespace>:: oo ::delegate`, when there is one.
    fn delegate_of(&self, cls: Id) -> Option<Id> {
        let name = format!("{}:: oo ::delegate", self.ns_of(cls));
        let id = *self.by_command.get(store_key(&name))?;
        self.is_class(id).then_some(id)
    }

    /// `MixinClassDelegates`: the delegate takes the delegates of `cls`'s
    /// superclasses as superclasses (`SetDelegateSuperclasses`) and is mixed
    /// into `cls` itself (`InstallDelegateAsMixin`).
    fn mixin_class_delegates(&mut self, cls: Id, delegate: Id) {
        if !self.alive(cls) || !self.alive(delegate) || !self.is_class(delegate) {
            return;
        }
        let supers = self.class(cls).superclasses.clone();
        for s in supers {
            let Some(sd) = self.delegate_of(s) else {
                continue;
            };
            if !self.class(delegate).superclasses.contains(&sd) {
                self.class_mut(delegate).superclasses.push(sd);
                self.class_mut(sd).subclasses.push(delegate);
            }
        }
        if !self.obj(cls).mixins.contains(&delegate) {
            self.obj_mut(cls).mixins.push(delegate);
            self.class_mut(delegate).instances.push(cls);
        }
    }

    /// `TclOOLookupObjectVar`'s TIP 500 step: the name `name` of object `obj`
    /// stands for in the calling method — a private variable of the method's
    /// declaring object, or of its declaring class when `obj` is an instance
    /// of that class or mixes it in — or `name` itself.
    fn private_var_name(&self, obj: Id, name: &str) -> String {
        let Some(Frame::Method { chain, index, .. }) = self.stack.last() else {
            return name.to_string();
        };
        let (vars, epoch) = match chain.calls[*index].declarer {
            Declarer::Object(o) if o == obj && self.alive(o) => {
                (&self.obj(o).private_vars, self.obj(o).private_epoch)
            }
            Declarer::Class(c) if self.alive(c) && self.alive(obj) => {
                let o = self.obj(obj);
                let instance =
                    self.reachable(c, o.class) || o.mixins.iter().any(|&m| self.reachable(c, m));
                if !instance {
                    return name.to_string();
                }
                (&self.class(c).private_vars, self.class(c).private_epoch)
            }
            _ => return name.to_string(),
        };
        match vars.iter().any(|v| v == name) {
            true => private_name(epoch, name),
            false => name.to_string(),
        }
    }

    /// The namespace a run-time call compiled in `ns` was made from: `ns`
    /// itself, or for [`METHOD_NS`] the running object's namespace.
    pub(crate) fn map_ns(&self, ns: &str) -> String {
        if ns == METHOD_NS {
            return match self.current_method() {
                Some((obj, ..)) if self.alive(obj) => self.obj(obj).ns.clone(),
                _ => "::".to_string(),
            };
        }
        ns.to_string()
    }

    /// `Tcl_GetObjectFromObj`: the object a name refers to from namespace `ns`.
    fn lookup(&self, name: &str, ns: &str) -> Option<Id> {
        if !name.starts_with("::") && ns != "::" {
            if let Some(&id) = self.by_command.get(store_key(&resolve(ns, name))) {
                return Some(id);
            }
        }
        self.by_command
            .get(store_key(&resolve("::", name)))
            .copied()
    }

    /// `TclOOIsReachable`: whether `target` is `start` or reachable from it
    /// through superclasses or mixins.
    fn reachable(&self, target: Id, start: Id) -> bool {
        if target == start {
            return true;
        }
        let c = self.class(start);
        c.superclasses
            .iter()
            .chain(c.mixins.iter())
            .any(|&s| self.reachable(target, s))
    }

    /// Bump the namespace counter for one new object, skipping names an
    /// existing namespace already holds.
    fn next_ns(&mut self, reg: &Registry) -> (String, u64) {
        loop {
            self.ns_count += 1;
            let ns = format!("::oo::Obj{}", self.ns_count);
            if !reg.exists(&ns) {
                return (ns, self.ns_count);
            }
        }
    }

    /// `AllocObject` and `TclNewObjectInstanceCommon`: a new instance of
    /// `class` named `command` (fully qualified), or `::oo::Obj<n>` when none
    /// is given.
    fn alloc(
        &mut self,
        reg: &mut Registry,
        class: Id,
        command: Option<String>,
        ns: Option<String>,
    ) -> Id {
        let (ns, creation_id) = match ns {
            Some(ns) => {
                self.ns_count += 1;
                (ns, self.ns_count)
            }
            None => self.next_ns(reg),
        };
        let makes_class = self.reachable(self.class_cls, class);
        let command = command.unwrap_or_else(|| ns.clone());
        let id = self.objects.len();
        self.objects.push(Some(Object {
            command: command.clone(),
            ns: ns.clone(),
            class,
            creation_id,
            methods: BTreeMap::new(),
            mixins: Vec::new(),
            filters: Vec::new(),
            variables: Vec::new(),
            private_vars: Vec::new(),
            private_epoch: 0,
            properties: Props::default(),
            class_data: makes_class.then(|| ClassData {
                superclasses: vec![self.object_cls],
                ..ClassData::default()
            }),
            destructing: false,
            filter_handling: false,
        }));
        self.class_mut(class).instances.push(id);
        if makes_class {
            let root = self.object_cls;
            self.class_mut(root).subclasses.push(id);
        }
        self.by_command.insert(store_key(&command).to_string(), id);
        self.by_ns.insert(ns.clone(), id);
        reg.ensure(&ns);
        reg.define(&command);
        reg.define(&format!("{ns}::my"));
        reg.define(&format!("{ns}::myclass"));
        id
    }
}

// ── call chains ──────────────────────────────────────────────────────────

struct Builder {
    obj: Id,
    calls: Vec<Call>,
    filter_len: usize,
}

impl Oo {
    /// `AddMethodToCallChain`.
    #[allow(clippy::too_many_arguments)]
    fn add_method(
        &self,
        b: &mut Builder,
        m: &Method,
        name: &str,
        declarer: Declarer,
        filter: bool,
        filter_decl: Option<Id>,
        flags: u32,
    ) {
        let Some(imp) = &m.imp else {
            return;
        };
        if !mixin_consistent(flags) {
            return;
        }
        // Methods come as late in the chain as possible: one already present is
        // moved to the end rather than added twice.
        for i in b.filter_len..b.calls.len() {
            if Arc::ptr_eq(&b.calls[i].imp, imp) && b.calls[i].filter == filter {
                let old = b.calls.remove(i);
                b.calls.push(Call {
                    imp: Arc::clone(imp),
                    name: name.to_string(),
                    declarer,
                    filter,
                    filter_declarer: old.filter_declarer,
                    private: m.flags & TRUE_PRIVATE_METHOD != 0,
                });
                return;
            }
        }
        b.calls.push(Call {
            imp: Arc::clone(imp),
            name: name.to_string(),
            declarer,
            filter,
            filter_declarer: filter_decl,
            private: m.flags & TRUE_PRIVATE_METHOD != 0,
        });
    }

    /// `AddSimpleChainToCallContext`.
    fn add_simple_chain(
        &self,
        b: &mut Builder,
        context_cls: Option<Id>,
        name: &str,
        filter: bool,
        mut flags: u32,
        filter_decl: Option<Id>,
    ) -> bool {
        let o = self.obj(b.obj);
        let mut found_private = false;
        let mut blocked = false;
        if flags & (KNOWN_STATE | SPECIAL) == 0 {
            if let Some(m) = o.methods.get(name) {
                if m.flags & TRUE_PRIVATE_METHOD == 0 {
                    if flags & PUBLIC_METHOD != 0 {
                        if m.flags & PUBLIC_METHOD == 0 {
                            blocked = true;
                        } else {
                            flags |= DEFINITE_PUBLIC;
                        }
                    } else {
                        flags |= DEFINITE_PROTECTED;
                    }
                }
            }
        }
        if flags & SPECIAL == 0 {
            for &mixin in &o.mixins {
                if let Some(cc) = context_cls {
                    found_private |= self.add_privates_from_class_chain(
                        b,
                        mixin,
                        cc,
                        name,
                        filter,
                        flags | TRAVERSED_MIXIN,
                        filter_decl,
                    );
                }
                found_private |= self.add_simple_class_chain(
                    b,
                    mixin,
                    name,
                    filter,
                    flags | TRAVERSED_MIXIN,
                    filter_decl,
                );
            }
            if !blocked {
                if let Some(m) = o.methods.get(name) {
                    if m.flags & TRUE_PRIVATE_METHOD == 0 {
                        self.add_method(
                            b,
                            m,
                            name,
                            Declarer::Object(b.obj),
                            filter,
                            filter_decl,
                            flags,
                        );
                    }
                }
            }
        }
        let cls = o.class;
        if let Some(cc) = context_cls {
            found_private |=
                self.add_privates_from_class_chain(b, cls, cc, name, filter, flags, filter_decl);
        }
        if !blocked {
            found_private |= self.add_simple_class_chain(b, cls, name, filter, flags, filter_decl);
        }
        found_private
    }

    /// `AddSimpleClassChainToCallContext`.
    fn add_simple_class_chain(
        &self,
        b: &mut Builder,
        mut cls: Id,
        name: &str,
        filter: bool,
        mut flags: u32,
        filter_decl: Option<Id>,
    ) -> bool {
        let mut danger = false;
        loop {
            let c = self.class(cls);
            for &mixin in &c.mixins {
                danger |= self.add_simple_class_chain(
                    b,
                    mixin,
                    name,
                    filter,
                    flags | TRAVERSED_MIXIN,
                    filter_decl,
                );
            }
            if flags & CONSTRUCTOR != 0 {
                if let Some(m) = &c.constructor {
                    self.add_method(
                        b,
                        m,
                        "<constructor>",
                        Declarer::Class(cls),
                        filter,
                        filter_decl,
                        flags,
                    );
                }
            } else if flags & DESTRUCTOR != 0 {
                if let Some(m) = &c.destructor {
                    self.add_method(
                        b,
                        m,
                        "<destructor>",
                        Declarer::Class(cls),
                        filter,
                        filter_decl,
                        flags,
                    );
                }
            } else if let Some(m) = c.methods.get(name) {
                if m.flags & TRUE_PRIVATE_METHOD != 0 {
                    danger = true;
                } else {
                    if flags & KNOWN_STATE == 0 {
                        if flags & PUBLIC_METHOD != 0 {
                            if m.flags & PUBLIC_METHOD == 0 {
                                return danger;
                            }
                            flags |= DEFINITE_PUBLIC;
                        } else {
                            flags |= DEFINITE_PROTECTED;
                        }
                    }
                    self.add_method(b, m, name, Declarer::Class(cls), filter, filter_decl, flags);
                }
            }
            match c.superclasses.as_slice() {
                [] => return danger,
                [only] => cls = *only,
                many => {
                    for &s in many {
                        danger |=
                            self.add_simple_class_chain(b, s, name, filter, flags, filter_decl);
                    }
                    return danger;
                }
            }
        }
    }

    /// `AddPrivatesFromClassChainToCallContext`.
    #[allow(clippy::too_many_arguments)]
    fn add_privates_from_class_chain(
        &self,
        b: &mut Builder,
        mut cls: Id,
        context_cls: Id,
        name: &str,
        filter: bool,
        flags: u32,
        filter_decl: Option<Id>,
    ) -> bool {
        loop {
            let c = self.class(cls);
            for &mixin in &c.mixins {
                if self.add_privates_from_class_chain(
                    b,
                    mixin,
                    context_cls,
                    name,
                    filter,
                    flags | TRAVERSED_MIXIN,
                    filter_decl,
                ) {
                    return true;
                }
            }
            if cls == context_cls {
                if let Some(m) = c.methods.get(name) {
                    if m.flags & TRUE_PRIVATE_METHOD != 0 {
                        self.add_method(
                            b,
                            m,
                            name,
                            Declarer::Class(cls),
                            filter,
                            filter_decl,
                            flags,
                        );
                        return true;
                    }
                }
            }
            match c.superclasses.as_slice() {
                [] => return false,
                [only] => cls = *only,
                many => {
                    for &s in many {
                        if self.add_privates_from_class_chain(
                            b,
                            s,
                            context_cls,
                            name,
                            filter,
                            flags,
                            filter_decl,
                        ) {
                            return true;
                        }
                    }
                    return false;
                }
            }
        }
    }

    /// `AddClassFiltersToCallContext`.
    fn add_class_filters(
        &self,
        b: &mut Builder,
        mut cls: Id,
        done: &mut HashSet<String>,
        flags: u32,
    ) {
        let cleared = flags & !(TRAVERSED_MIXIN | OBJECT_MIXIN | BUILDING_MIXINS);
        loop {
            let c = self.class(cls);
            for &mixin in &c.mixins {
                self.add_class_filters(b, mixin, done, flags | TRAVERSED_MIXIN);
            }
            if mixin_consistent(flags) {
                for f in &c.filters {
                    if done.insert(f.clone()) {
                        self.add_simple_chain(
                            b,
                            None,
                            f,
                            true,
                            cleared | BUILDING_MIXINS,
                            Some(cls),
                        );
                        self.add_simple_chain(b, None, f, true, cleared, Some(cls));
                    }
                }
            }
            match c.superclasses.as_slice() {
                [] => return,
                [only] => cls = *only,
                many => {
                    for &s in many {
                        self.add_class_filters(b, s, done, flags);
                    }
                    return;
                }
            }
        }
    }

    /// `TclOOGetCallContext`: the chain for calling `name` on `obj` (`None`
    /// for a constructor or destructor), or `None` when nothing implements it
    /// — not even `unknown`.
    fn chain_for(
        &self,
        obj: Id,
        name: Option<&str>,
        flags: u32,
        context_obj: Option<Id>,
        context_cls: Option<Id>,
    ) -> Option<Chain> {
        let o = self.obj(obj);
        let filter_handling = o.filter_handling;
        let mut chain_flags = flags & (PUBLIC_METHOD | CONSTRUCTOR | DESTRUCTOR);
        if filter_handling {
            chain_flags |= FILTER_HANDLING;
        }
        let mut b = Builder {
            obj,
            calls: Vec::new(),
            filter_len: 0,
        };
        if flags & FORCE_UNKNOWN != 0 {
            self.add_simple_chain(&mut b, None, "unknown", false, BUILDING_MIXINS, None);
            self.add_simple_chain(&mut b, None, "unknown", false, 0, None);
            if b.calls.is_empty() {
                return None;
            }
            return Some(Chain {
                calls: b.calls,
                flags: chain_flags | OO_UNKNOWN_METHOD,
            });
        }
        if flags & SPECIAL == 0 && !filter_handling {
            let mut done = HashSet::new();
            for &mixin in &o.mixins {
                self.add_class_filters(
                    &mut b,
                    mixin,
                    &mut done,
                    TRAVERSED_MIXIN | BUILDING_MIXINS | OBJECT_MIXIN,
                );
                self.add_class_filters(&mut b, mixin, &mut done, OBJECT_MIXIN);
            }
            for f in &o.filters {
                self.add_simple_chain(&mut b, context_cls, f, true, BUILDING_MIXINS, None);
                self.add_simple_chain(&mut b, context_cls, f, true, 0, None);
            }
            self.add_class_filters(&mut b, o.class, &mut done, BUILDING_MIXINS);
            self.add_class_filters(&mut b, o.class, &mut done, 0);
        }
        let count = b.calls.len();
        b.filter_len = count;
        let name = name.unwrap_or("");
        if context_obj == Some(obj) {
            if let Some(m) = o.methods.get(name) {
                if m.flags & TRUE_PRIVATE_METHOD != 0 {
                    self.add_method(&mut b, m, name, Declarer::Object(obj), false, None, flags);
                }
            }
        }
        self.add_simple_chain(
            &mut b,
            context_cls,
            name,
            false,
            flags | BUILDING_MIXINS,
            None,
        );
        self.add_simple_chain(&mut b, context_cls, name, false, flags, None);
        if count == b.calls.len() {
            if flags & SPECIAL != 0 {
                return None;
            }
            self.add_simple_chain(&mut b, None, "unknown", false, BUILDING_MIXINS, None);
            self.add_simple_chain(&mut b, None, "unknown", false, 0, None);
            chain_flags |= OO_UNKNOWN_METHOD;
            if count == b.calls.len() {
                return None;
            }
        }
        Some(Chain {
            calls: b.calls,
            flags: chain_flags,
        })
    }

    /// The declaring object of a chain entry, for `self next` and the rest.
    fn declarer_name(&self, d: Declarer) -> String {
        match d {
            Declarer::Class(id) | Declarer::Object(id) => self.name(id),
        }
    }

    /// `TclOORenderCallChain`: `{type name declarer impl} …`.
    fn render_chain(&self, chain: &Chain) -> String {
        let items: Vec<String> = chain
            .calls
            .iter()
            .map(|c| {
                let kind = if c.filter {
                    "filter"
                } else if chain.flags & OO_UNKNOWN_METHOD != 0 {
                    "unknown"
                } else if c.private {
                    "private"
                } else {
                    "method"
                };
                let name = if chain.flags & CONSTRUCTOR != 0 {
                    "<constructor>".to_string()
                } else if chain.flags & DESTRUCTOR != 0 {
                    "<destructor>".to_string()
                } else {
                    c.name.clone()
                };
                let declarer = match c.declarer {
                    Declarer::Object(_) => "object".to_string(),
                    Declarer::Class(id) => self.name(id),
                };
                crate::list::join(&[
                    kind.to_string(),
                    name,
                    declarer,
                    method_type(&c.imp, &c.name),
                ])
            })
            .collect();
        crate::list::join(&items)
    }
}

/// The fourth word of a rendered chain entry, and `info … methodtype`.
fn method_type(imp: &Imp, name: &str) -> String {
    match imp {
        Imp::Proc(_) => "method".to_string(),
        Imp::Forward(_) => "forward".to_string(),
        Imp::Builtin(_) => format!("core method: \"{name}\""),
        Imp::Property { setter: false, .. } => "PropertyGetter".to_string(),
        Imp::Property { setter: true, .. } => "PropertySetter".to_string(),
    }
}

// ── errors ───────────────────────────────────────────────────────────────

fn coded_err(msg: impl Into<String>, code: &[&str]) -> TclError {
    TclError {
        errorcode: Some(crate::list::join(code)),
        ..TclError::plain(msg)
    }
}

fn oo_err(msg: impl Into<String>, what: &str) -> TclError {
    coded_err(msg, &["TCL", "OO", what])
}

fn wrong_args(usage: &str) -> TclError {
    coded_err(
        format!("wrong # args: should be \"{usage}\""),
        &["TCL", "WRONGARGS"],
    )
}

/// `Tcl_WrongNumArgs`: the words already consumed, then the rest of the usage.
fn usage_of(prefix: &str, rest: &str) -> String {
    match (prefix.is_empty(), rest.is_empty()) {
        (_, true) => prefix.to_string(),
        (true, false) => rest.to_string(),
        (false, false) => format!("{prefix} {rest}"),
    }
}

fn not_an_object(name: &str) -> TclError {
    coded_err(
        format!("{name} does not refer to an object"),
        &["TCL", "LOOKUP", "OBJECT", name],
    )
}

/// `Tcl_GetIndexFromObj`'s refusal: `bad option "x": must be a, b, or c`.
fn bad_index(what: &str, given: &str, table: &[&str], ambiguous: bool) -> TclError {
    let list = match table {
        [] => String::new(),
        [only] => (*only).to_string(),
        // `Tcl_GetIndexFromObj` puts a comma before the `or` only in a
        // list of three or more.
        [first, last] => format!("{first} or {last}"),
        [init @ .., last] => format!("{}, or {last}", init.join(", ")),
    };
    let adjective = if ambiguous { "ambiguous" } else { "bad" };
    coded_err(
        format!("{adjective} {what} \"{given}\": must be {list}"),
        &["TCL", "LOOKUP", "INDEX", what, given],
    )
}

/// `Tcl_GetIndexFromObj` with prefix matching: a prefix of several entries —
/// the empty string is one of every entry — is ambiguous rather than bad.
fn index_of<'a>(what: &str, given: &str, table: &[&'a str]) -> Result<&'a str, TclError> {
    if let Some(exact) = table.iter().find(|t| **t == given) {
        return Ok(exact);
    }
    let hits: Vec<&&str> = table.iter().filter(|t| t.starts_with(given)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(bad_index(what, given, table, false)),
        _ => Err(bad_index(what, given, table, true)),
    }
}

fn val(s: impl Into<String>) -> Value {
    Value::Str(Arc::new(s.into()))
}

fn strings(args: &[Value]) -> Vec<String> {
    args.iter().map(to_tcl_string).collect()
}

fn lock(interp: &Shared) -> std::sync::MutexGuard<'_, crate::runtime::State> {
    interp.lock().expect("interpreter lock")
}

// ── the commands an interpreter starts with ──────────────────────────────

/// The `oo::define` subcommands (`defineCmds`, `tclOODefineCmds.c`), sorted.
const DEFINE_CMDS: &[&str] = &[
    "classmethod",
    "constructor",
    "definitionnamespace",
    "deletemethod",
    "destructor",
    "export",
    "filter",
    "forward",
    "initialise",
    "initialize",
    "method",
    "mixin",
    "private",
    "renamemethod",
    "self",
    "superclass",
    "unexport",
    "variable",
];

/// The `oo::objdefine` subcommands (`objdefCmds`), sorted.
const OBJDEFINE_CMDS: &[&str] = &[
    "class",
    "deletemethod",
    "export",
    "filter",
    "forward",
    "method",
    "mixin",
    "private",
    "renamemethod",
    "self",
    "unexport",
    "variable",
];

/// Record the object system's namespaces and commands in the registry, so
/// `namespace exists`, `info commands` and `namespace which` see them.
pub(crate) fn install(reg: &mut Registry) {
    for ns in [
        "::oo",
        "::oo::define",
        "::oo::objdefine",
        "::oo::Helpers",
        "::oo::Obj1",
        "::oo::Obj2",
        "::oo::Obj3",
        "::oo::Obj4",
        "::oo::Obj5",
        "::oo::Obj6",
        "::oo::Obj7",
        "::oo::Obj8",
        "::oo::Obj9",
        "::oo::Obj10",
        "::oo::Obj15",
        "::oo::Obj16",
        "::oo::Obj17",
    ] {
        reg.ensure(ns);
    }
    for cmd in [
        "::oo::object",
        "::oo::class",
        "::oo::copy",
        "::oo::define",
        "::oo::objdefine",
        "::oo::InfoObject",
        "::oo::InfoClass",
        "::oo::DelegateName",
        "::oo::UnknownDefinition",
        "::oo::Helpers::self",
        "::oo::Helpers::next",
        "::oo::Helpers::nextto",
        "::oo::Helpers::callback",
        "::oo::Helpers::mymethod",
        "::oo::Helpers::classvariable",
        "::oo::Helpers::link",
        "::oo::Obj1::my",
        "::oo::Obj2::my",
        "::oo::Obj1::myclass",
        "::oo::Obj2::myclass",
        "::oo::singleton",
        "::oo::SingletonInstance",
        "::oo::abstract",
        "::oo::Obj15::my",
        "::oo::Obj16::my",
        "::oo::Obj17::my",
        "::oo::Obj15::myclass",
        "::oo::Slot",
        "::oo::Obj16::myclass",
        "::oo::Obj17::myclass",
    ] {
        reg.define(cmd);
    }
    // `oo::Slot`, the standard slots and the configurable support classes.
    for n in (3..=14).chain([18, 19]) {
        reg.ensure(&format!("::oo::Obj{n}"));
        reg.define(&format!("::oo::Obj{n}::my"));
        reg.define(&format!("::oo::Obj{n}::myclass"));
    }
    for cmd in [
        "::oo::configurable",
        "::oo::configuresupport::configurable",
        "::oo::configuresupport::readableproperties",
        "::oo::configuresupport::writableproperties",
        "::oo::configuresupport::objreadableproperties",
        "::oo::configuresupport::objwritableproperties",
    ] {
        reg.define(cmd);
    }
    for space in [CFG_CLASS_NS, CFG_OBJECT_NS] {
        reg.define(&format!("{space}::property"));
        reg.define(&format!("{space}::properties"));
    }
    for cmd in DEFINE_CMDS {
        reg.define(&format!("::oo::define::{cmd}"));
    }
    for cmd in OBJDEFINE_CMDS {
        reg.define(&format!("::oo::objdefine::{cmd}"));
    }
}

/// What a command name reaches in the object system.
enum Target {
    Object {
        id: Id,
        public: bool,
    },
    SelfCmd,
    Next,
    NextTo,
    Define {
        class: bool,
    },
    Copy,
    InfoObject,
    InfoClass,
    CurrentNs,
    DefineCmd(&'static str),
    /// `callback`, `mymethod`, `classvariable` and `link` of `oo::Helpers`,
    /// and the check a compiled `classvariable` runs.
    Helper(&'static str),
    /// An object's `myclass`: its class, called with private access.
    MyClass(Id),
    /// A command `link` made: the object's `my` with the method named.
    Link(Id, String),
    /// `oo::DelegateName`.
    DelegateName,
    /// `oo::UnknownDefinition`, the unknown handler of the definition
    /// namespaces.
    UnknownDefinition,
}

/// A unique prefix of one of `table`'s names, as `TclOOUnknownDefinition`
/// accepts one inside a definition script.
fn define_prefix(given: &str, table: &[&'static str]) -> Option<&'static str> {
    if let Some(exact) = table.iter().find(|t| **t == given) {
        return Some(exact);
    }
    let hits: Vec<&&'static str> = table
        .iter()
        .filter(|t| !given.is_empty() && t.starts_with(given))
        .collect();
    match hits.as_slice() {
        [one] => Some(**one),
        _ => None,
    }
}

impl Oo {
    fn resolve_command(&self, name: &str, ns: &str) -> Option<Target> {
        if let Some(id) = self.lookup(name, ns) {
            return Some(Target::Object { id, public: true });
        }
        // An object's `my`, written qualified or reached from inside the
        // object's namespace.
        let here = if name.starts_with("::") { "::" } else { ns };
        let qualified = resolve(here, name);
        if tail(&qualified) == "my" {
            if let Some(&id) = self.by_ns.get(qualifiers(&qualified)) {
                return Some(Target::Object { id, public: false });
            }
        }
        if tail(&qualified) == "myclass" {
            if let Some(&id) = self.by_ns.get(qualifiers(&qualified)) {
                return Some(Target::MyClass(id));
            }
        }
        // A command `link` made, looked up as `Tcl_FindCommand` would: the
        // namespace written in, then the root.
        for at in [qualified.as_str(), &resolve("::", name)] {
            if let Some((id, method)) = self.links.get(store_key(at)) {
                return Some(Target::Link(*id, method.clone()));
            }
        }
        let key = store_key(&resolve("::", name)).to_string();
        match key.as_str() {
            "oo::define" => return Some(Target::Define { class: true }),
            "oo::objdefine" => return Some(Target::Define { class: false }),
            "oo::copy" => return Some(Target::Copy),
            "oo::InfoObject" => return Some(Target::InfoObject),
            "oo::InfoClass" => return Some(Target::InfoClass),
            "oo::DelegateName" => return Some(Target::DelegateName),
            "oo::UnknownDefinition" => return Some(Target::UnknownDefinition),
            "oo::Helpers::callback" => return Some(Target::Helper("callback")),
            "oo::Helpers::mymethod" => return Some(Target::Helper("mymethod")),
            "oo::Helpers::classvariable" => return Some(Target::Helper("classvariable")),
            "oo::Helpers::link" => return Some(Target::Helper("link")),
            k if k == store_key(&resolve("::", CLASSVAR_CHECK_CMD)) => {
                return Some(Target::Helper("classvariable-check"))
            }
            k if k == store_key(&resolve("::", CURRENT_NS_CMD)) => return Some(Target::CurrentNs),
            _ => {}
        }
        match self.stack.last()? {
            Frame::Method { obj, .. } => match key.as_str() {
                "my" => Some(Target::Object {
                    id: *obj,
                    public: false,
                }),
                "self" | "oo::Helpers::self" => Some(Target::SelfCmd),
                "next" | "oo::Helpers::next" => Some(Target::Next),
                "nextto" | "oo::Helpers::nextto" => Some(Target::NextTo),
                "callback" => Some(Target::Helper("callback")),
                "mymethod" => Some(Target::Helper("mymethod")),
                "classvariable" => Some(Target::Helper("classvariable")),
                "link" => Some(Target::Helper("link")),
                "myclass" => Some(Target::MyClass(*obj)),
                _ => None,
            },
            Frame::Define { space, .. } => {
                // Only `::oo::define` and `::oo::objdefine` hold the definition
                // commands; a definition namespace holds what was put in it.
                let prefix = format!("{}::", store_key(space));
                let simple = key.strip_prefix(prefix.as_str()).unwrap_or(&key);
                if simple.contains("::") {
                    return None;
                }
                define_lookup(space, simple).map(Target::DefineCmd)
            }
        }
    }
}

/// The namespace a call compiled in `ns` runs in — [`METHOD_NS`] mapped to the
/// running object's namespace.
pub(crate) fn current_ns(interp: &Shared, ns: &str) -> String {
    if ns == METHOD_NS {
        lock(interp).oo.map_ns(ns)
    } else {
        ns.to_string()
    }
}

/// Run `name` if the object system answers to it: an object's command, `my`,
/// `self`, `next`, `oo::define` and the rest. `None` when it does not, so the
/// caller goes on to report `invalid command name`. `ns` is the namespace the
/// call was written in, when that is not the global one.
pub(crate) fn try_dispatch(
    interp: &Shared,
    vm: &mut VM,
    name: &str,
    ns: Option<&str>,
    args: &[Value],
) -> Option<Result<Value, TclError>> {
    let (target, ns) = {
        let state = lock(interp);
        let ns = state.oo.map_ns(ns.unwrap_or("::"));
        (state.oo.resolve_command(name, &ns)?, ns)
    };
    Some(match target {
        Target::Object { id, public } => invoke_object(interp, vm, id, public, name, &ns, args),
        Target::SelfCmd => self_cmd(interp, name, args),
        Target::Next => next_cmd(interp, vm, name, &ns, args),
        Target::NextTo => nextto_cmd(interp, vm, name, &ns, args),
        Target::Define { class } => define_cmd(interp, vm, class, name, &ns, args),
        Target::Copy => copy_cmd(interp, vm, &ns, args),
        // `info object vars` reads the variable table, so what the calling
        // chunk has assigned is written back first.
        Target::InfoObject => {
            crate::runtime::with_written_back(interp, vm, |interp| info_object(interp, &ns, args))
        }
        Target::InfoClass => info_class(interp, &ns, args),
        Target::CurrentNs => Ok(val(lock(interp).oo.map_ns(METHOD_NS))),
        Target::DefineCmd(cmd) => define_sub(interp, vm, cmd, name, &strings(args)),
        Target::Helper(which) => helper(interp, which, name, args),
        Target::DelegateName => match args {
            [class] => {
                let state = lock(interp);
                class_of(&state.oo, &to_tcl_string(class), &ns)
                    .map(|c| val(format!("{}:: oo ::delegate", state.oo.ns_of(c))))
            }
            _ => Err(wrong_args(&format!("{name} class"))),
        },
        Target::UnknownDefinition => unknown_definition(interp, vm, args),
        Target::MyClass(id) => {
            if args.is_empty() {
                return Some(Err(wrong_args(&format!("{name} methodName ?arg ...?"))));
            }
            let class = lock(interp).oo.obj(id).class;
            invoke_object(interp, vm, class, false, name, &ns, args)
        }
        Target::Link(id, method) => {
            let mut words = vec![val(method)];
            words.extend_from_slice(args);
            invoke_object(interp, vm, id, false, name, &ns, &words)
        }
    })
}

// ── invoking methods ─────────────────────────────────────────────────────

/// The method context a call is made from, for TIP 500 private methods.
fn calling_context(oo: &Oo) -> (Option<Id>, Option<Id>) {
    match oo.stack.last() {
        Some(Frame::Method { chain, index, .. }) => match chain.calls[*index].declarer {
            Declarer::Object(o) => (Some(o), None),
            Declarer::Class(c) => (None, Some(c)),
        },
        _ => (None, None),
    }
}

/// `TclOOObjectCmdCore`: `written` is the command word as the call spelled it,
/// which is what a `wrong # args` repeats.
fn invoke_object(
    interp: &Shared,
    vm: &mut VM,
    id: Id,
    public: bool,
    written: &str,
    ns: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let flags = if public { PUBLIC_METHOD } else { 0 };
    let method = args.first().map(to_tcl_string);
    let chain = {
        let state = lock(interp);
        // `my` of an object deleted while its method runs: the command is
        // gone with the object.
        if !state.oo.alive(id) {
            return Err(coded_err(
                format!("invalid command name \"{written}\""),
                &["TCL", "LOOKUP", "COMMAND", written],
            ));
        }
        let (cobj, ccls) = calling_context(&state.oo);
        match &method {
            None => state
                .oo
                .chain_for(id, None, flags | FORCE_UNKNOWN, cobj, ccls),
            Some(m) => state.oo.chain_for(id, Some(m), flags, cobj, ccls),
        }
    };
    let Some(chain) = chain else {
        let m = method.unwrap_or_default();
        return Err(coded_err(
            format!("impossible to invoke method \"{m}\": no defined method or unknown method"),
            &["TCL", "LOOKUP", "METHOD", &m],
        ));
    };
    // An `unknown` handler is passed the method name as its first argument.
    let (usage, rest) = match &method {
        Some(m) if chain.flags & OO_UNKNOWN_METHOD == 0 => (format!("{written} {m}"), &args[1..]),
        _ => (written.to_string(), args),
    };
    invoke_context(interp, vm, id, Arc::new(chain), 0, rest, &usage, ns)
}

/// `TclOOInvokeContext`: run the chain entry at `index`.
#[allow(clippy::too_many_arguments)]
fn invoke_context(
    interp: &Shared,
    vm: &mut VM,
    obj: Id,
    chain: Arc<Chain>,
    index: usize,
    args: &[Value],
    usage: &str,
    ns: &str,
) -> Result<Value, TclError> {
    let call = chain.calls[index].clone();
    // A chain goes on running after its object is deleted — `next` after
    // `rename [self] {}` still reaches the next implementation.
    let saved = {
        let mut state = lock(interp);
        state.oo.alive(obj).then(|| {
            let o = state.oo.obj_mut(obj);
            let saved = o.filter_handling;
            o.filter_handling = call.filter || chain.flags & FILTER_HANDLING != 0;
            saved
        })
    };
    let frame = Frame::Method {
        obj,
        chain: Arc::clone(&chain),
        index,
    };
    let result = match &*call.imp {
        Imp::Proc(p) => run_proc(interp, vm, frame, &call, chain.flags, p, args, usage),
        Imp::Forward(prefix) => run_forward(interp, vm, obj, prefix, args, usage),
        Imp::Builtin(b) => run_builtin(interp, vm, frame, *b, args, usage, ns),
        Imp::Property { var, setter } => {
            property_access(interp, vm, obj, var, *setter, args, usage)
        }
    };
    let mut state = lock(interp);
    if let (Some(saved), true) = (saved, state.oo.alive(obj)) {
        state.oo.obj_mut(obj).filter_handling = saved;
    }
    result
}

/// Run `body` with `frame` as the innermost method context.
fn with_frame<T>(interp: &Shared, frame: Frame, body: impl FnOnce() -> T) -> T {
    let depth = {
        let mut state = lock(interp);
        state.oo.stack.push(frame);
        state.oo.stack.len() - 1
    };
    let out = body();
    lock(interp).oo.stack.truncate(depth);
    out
}

/// The context line `errorInfo` reports for a method body, after
/// `TclOOMethodTypes`' `procErrorProc`.
fn describe(oo: &Oo, call: &Call, chain_flags: u32) -> String {
    let (kind, who) = match call.declarer {
        Declarer::Class(c) => ("class", oo.name(c)),
        Declarer::Object(o) => ("object", oo.name(o)),
    };
    if chain_flags & CONSTRUCTOR != 0 {
        format!("{kind} \"{who}\" constructor")
    } else if chain_flags & DESTRUCTOR != 0 {
        format!("{kind} \"{who}\" destructor")
    } else {
        format!("{kind} \"{who}\" method \"{}\"", call.name)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_proc(
    interp: &Shared,
    vm: &mut VM,
    frame: Frame,
    call: &Call,
    chain_flags: u32,
    p: &ProcImp,
    args: &[Value],
    usage: &str,
) -> Result<Value, TclError> {
    let (vars, desc) = {
        let state = lock(interp);
        let oo = &state.oo;
        let vars = match call.declarer {
            Declarer::Class(c) if oo.alive(c) => declared_vars(
                &oo.class(c).private_vars,
                &oo.class(c).variables,
                oo.class(c).private_epoch,
            ),
            Declarer::Object(o) if oo.alive(o) => declared_vars(
                &oo.obj(o).private_vars,
                &oo.obj(o).variables,
                oo.obj(o).private_epoch,
            ),
            _ => Vec::new(),
        };
        (vars, describe(oo, call, chain_flags))
    };
    let compiled = compile(interp, p, &vars, &desc)?;
    let actuals = crate::procs::actuals(usage, &p.sig, args).map_err(|msg| TclError {
        errorcode: Some("TCL WRONGARGS".to_string()),
        ..TclError::plain(msg)
    })?;
    // What `info level` names this activation by: the words of the command
    // that invoked the method (`o m 1`, `next 1`, `my n`, `C create o 1`),
    // as `objv` is to `InfoLevelCmd`, not the procedure the body compiled to.
    let mut words = crate::list::split(usage).unwrap_or_else(|_| vec![usage.to_string()]);
    words.extend(args.iter().map(to_tcl_string));
    METHOD_CALLS.with(|c| {
        c.borrow_mut()
            .push((crate::procs::body_of(&compiled), words))
    });
    let result = crate::runtime::with_written_back(interp, vm, |interp| {
        with_frame(interp, frame, || {
            crate::procs::call_compiled(interp, &compiled, actuals)
        })
    });
    METHOD_CALLS.with(|c| c.borrow_mut().pop());
    result
        // The namespace standing for the object's is not one a script can name, so
        // it never appears in what the script is told.
        .map_err(|mut e| {
            if e.msg.contains(METHOD_NS_KEY) {
                e.msg = e
                    .msg
                    .replace(&format!("::{METHOD_NS_KEY}"), "")
                    .replace(METHOD_NS_KEY, "");
            }
            e
        })
}

/// Where a method body starts — its chunk and entry point — and the words of
/// the command that invoked it.
type MethodCall = ((crate::procs::ChunkKey, usize), Vec<String>);

thread_local! {
    /// The method bodies running, innermost last.
    static METHOD_CALLS: std::cell::RefCell<Vec<MethodCall>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The words that invoked the running method whose body starts at `entry` of
/// `chunk` — what `info level` answers for that activation — or `None` when
/// the activation is no method body.
pub(crate) fn method_call_words(chunk: &fusevm::Chunk, entry: usize) -> Option<Vec<String>> {
    let key = (crate::procs::chunk_key(chunk), entry);
    METHOD_CALLS.with(|c| {
        c.borrow()
            .iter()
            .rev()
            .find(|(body, _)| *body == key)
            .map(|(_, words)| words.clone())
    })
}

/// A variable a method body declares on entry: its name, and for a private
/// one the name it is kept under in the object's namespace.
type DeclaredVar = (String, Option<String>);

/// What `ProcedureMethodCompiledVarConnect` resolves for a method of a declarer
/// with creation id `epoch`: its private variables first, each under
/// `PRIVATE_VARIABLE_PATTERN` (`"%d : %s"`), then its standard ones.
fn declared_vars(private: &[String], standard: &[String], epoch: u64) -> Vec<DeclaredVar> {
    let mut out: Vec<DeclaredVar> = private
        .iter()
        .map(|v| (v.clone(), Some(private_name(epoch, v))))
        .collect();
    for v in standard {
        if !out.iter().any(|(n, _)| n == v) {
            out.push((v.clone(), None));
        }
    }
    out
}

/// `PRIVATE_VARIABLE_PATTERN`: where a private variable of the declarer with
/// creation id `epoch` lives in an object's namespace.
fn private_name(epoch: u64, name: &str) -> String {
    format!("{epoch} : {name}")
}

/// The procedure a method body compiles to: the body, preceded by a
/// `variable` for each name the declarer declared that is not a parameter,
/// as a procedure of [`METHOD_NS`].
fn compile(
    interp: &Shared,
    p: &ProcImp,
    vars: &[DeclaredVar],
    desc: &str,
) -> Result<RuntimeProc, TclError> {
    if let Some((v, d, done)) = &*p.compiled.lock().expect("method lock") {
        if v == vars && d == desc {
            return Ok(done.clone());
        }
    }
    let params: HashSet<&str> = p.sig.params.iter().map(|x| x.name.as_str()).collect();
    let mut body = String::new();
    for (v, mangled) in vars {
        if params.contains(v.as_str()) {
            continue;
        }
        let words = match mangled {
            Some(full) => vec![PRIVATE_VAR_CMD, full.as_str(), v.as_str()],
            None => vec!["variable", v.as_str()],
        };
        body.push_str(&crate::list::join(&words));
        body.push(';');
    }
    body.push_str(&p.body);
    let name = format!("{METHOD_PROC_PREFIX}{desc}");
    let inner = crate::list::join(&["proc", name.as_str(), p.params.as_str(), body.as_str()]);
    let src = format!(
        "namespace eval {} {}",
        crate::list::quote(METHOD_NS, false),
        crate::list::quote(&inner, false)
    );
    crate::runtime::run_source(interp, &src)?;
    let fqn = resolve(METHOD_NS, &name);
    let done = {
        let mut state = lock(interp);
        let done = state.commands.remove(store_key(&fqn));
        state.ns.forget_namespace(METHOD_NS);
        done
    };
    let done =
        done.ok_or_else(|| TclError::plain(format!("method body for {desc} did not compile")))?;
    *p.compiled.lock().expect("method lock") =
        Some((vars.to_vec(), desc.to_string(), done.clone()));
    Ok(done)
}

/// A forwarded method: the prefix and then the call's arguments, as one
/// command, its name resolved from the object's namespace.
fn run_forward(
    interp: &Shared,
    vm: &mut VM,
    obj: Id,
    prefix: &[String],
    args: &[Value],
    usage: &str,
) -> Result<Value, TclError> {
    let mut words: Vec<Value> = prefix.iter().map(|w| val(w.as_str())).collect();
    words.extend_from_slice(args);
    let ns = lock(interp).oo.ns_of(obj);
    crate::procs::call_words_sync(interp, vm, &words, Some(&ns))
        .map_err(|e| forwarded_usage(e, prefix, usage))
}

/// A `wrong # args` from the command a forward reached names the words the
/// *caller* wrote, not the prefix — `TclInitRewriteEnsemble` replaces the
/// inserted words with the removed ones, so `forward len string length`
/// reports `::foo len string`.
fn forwarded_usage(mut e: TclError, prefix: &[String], outer: &str) -> TclError {
    const HEAD: &str = "wrong # args: should be \"";
    let Some(usage) = e.msg.strip_prefix(HEAD).and_then(|r| r.strip_suffix('"')) else {
        return e;
    };
    let Ok(words) = crate::list::split(usage) else {
        return e;
    };
    if words.len() < prefix.len() || words.first() != prefix.first() {
        return e;
    }
    let mut rewritten = outer.to_string();
    for w in &words[prefix.len()..] {
        rewritten.push(' ');
        rewritten.push_str(w);
    }
    e.msg = format!("{HEAD}{rewritten}\"");
    e.line = None;
    e
}

fn run_builtin(
    interp: &Shared,
    vm: &mut VM,
    frame: Frame,
    b: Builtin,
    args: &[Value],
    usage: &str,
    ns: &str,
) -> Result<Value, TclError> {
    let Frame::Method {
        obj,
        ref chain,
        index,
    } = frame
    else {
        unreachable!("a method runs in a method frame")
    };
    match b {
        Builtin::Destroy => {
            if !args.is_empty() {
                return Err(wrong_args(usage));
            }
            destroy_object(interp, vm, obj)?;
            Ok(val(""))
        }
        Builtin::Eval => {
            if args.is_empty() {
                return Err(wrong_args(&usage_of(usage, "arg ?arg ...?")));
            }
            let words = strings(args);
            let script = if words.len() == 1 {
                words[0].clone()
            } else {
                crate::cmd_list::concat(&words)
            };
            let obj_ns = lock(interp).oo.ns_of(obj);
            let src = format!(
                "namespace eval {} {}",
                crate::list::quote(&obj_ns, false),
                crate::list::quote(&script, false)
            );
            let who = if chain.flags & PUBLIC_METHOD != 0 {
                lock(interp).oo.name(obj)
            } else {
                "my".to_string()
            };
            let wrapper = format!("namespace eval {} ", crate::list::quote(&obj_ns, false));
            crate::runtime::with_written_back(interp, vm, |interp| {
                with_frame(interp, frame.clone(), || {
                    crate::runtime::run_source(interp, &src)
                })
            })
            .map_err(|e| {
                relabel(e, &wrapper, |line| {
                    format!("(in \"{who} eval\" script line {line})")
                })
            })
        }
        Builtin::Unknown => unknown_method(interp, obj, chain, args, usage),
        Builtin::Variable => link_variables(interp, vm, obj, args),
        Builtin::VarName => {
            let [name] = args else {
                return Err(wrong_args(&usage_of(usage, "varName")));
            };
            let state = lock(interp);
            let name = state.oo.private_var_name(obj, &to_tcl_string(name));
            Ok(val(format!("{}::{name}", state.oo.ns_of(obj))))
        }
        Builtin::Cloned => {
            let [origin] = args else {
                return Err(wrong_args(&usage_of(usage, "originObject")));
            };
            let origin = to_tcl_string(origin);
            let mut state = lock(interp);
            let from = state
                .oo
                .lookup(&origin, ns)
                .ok_or_else(|| not_an_object(&origin))?;
            let from_key = format!("{}::", store_key(&state.oo.obj(from).ns));
            let to_key = format!("{}::", store_key(&state.oo.ns_of(obj)));
            let copies: Vec<(String, Value)> = state
                .globals
                .iter()
                .filter_map(|(k, v)| {
                    let rest = k.strip_prefix(&from_key)?;
                    (!rest.contains("::")).then(|| (format!("{to_key}{rest}"), v.clone()))
                })
                .collect();
            state.globals.extend(copies);
            Ok(val(""))
        }
        Builtin::Create => {
            let Some(name) = args.first().map(to_tcl_string) else {
                return Err(wrong_args(&usage_of(usage, "objectName ?arg ...?")));
            };
            if name.is_empty() {
                return Err(oo_err("object name must not be empty", "EMPTY_NAME"));
            }
            let ctor_usage = format!("{usage} {}", crate::list::quote(&name, false));
            new_instance(
                interp,
                vm,
                obj,
                Some(&name),
                None,
                &args[1..],
                &ctor_usage,
                ns,
            )
        }
        Builtin::CreateNs => {
            if args.len() < 2 {
                return Err(wrong_args(&usage_of(
                    usage,
                    "objectName namespaceName ?arg ...?",
                )));
            }
            let name = to_tcl_string(&args[0]);
            let ns_name = to_tcl_string(&args[1]);
            if name.is_empty() {
                return Err(oo_err("object name must not be empty", "EMPTY_NAME"));
            }
            if ns_name.is_empty() {
                return Err(oo_err("namespace name must not be empty", "EMPTY_NAME"));
            }
            let ctor_usage = format!("{usage} {name} {ns_name}");
            new_instance(
                interp,
                vm,
                obj,
                Some(&name),
                Some(&ns_name),
                &args[2..],
                &ctor_usage,
                ns,
            )
        }
        Builtin::New => new_instance(interp, vm, obj, None, None, args, usage, ns),
        // `TclOO_Singleton_New`: the class's one instance if it has one, else
        // a new one with `oo::SingletonInstance` prepended to its mixins
        // (`MarkAsSingleton`).
        Builtin::SingletonNew => {
            let existing = {
                let state = lock(interp);
                let first = state.oo.class(obj).instances.first();
                first.map(|&id| state.oo.name(id))
            };
            if let Some(name) = existing {
                return Ok(val(name));
            }
            let made = new_instance(interp, vm, obj, None, None, args, usage, ns)?;
            let mut state = lock(interp);
            let oo = &mut state.oo;
            if let Some(&first) = oo.class(obj).instances.first() {
                oo.obj_mut(first).mixins.insert(0, SINGLETON_INSTANCE);
                oo.class_mut(SINGLETON_INSTANCE).instances.push(first);
            }
            Ok(made)
        }
        Builtin::SingletonDestroy => Err(oo_err("may not destroy a singleton object", "SINGLETON")),
        Builtin::SingletonCloned => Err(oo_err("may not clone a singleton object", "SINGLETON")),
        Builtin::SlotUnimplemented => Err(oo_err("unimplemented", "ABSTRACT_SLOT")),
        Builtin::SlotResolve => match args {
            [item] => Ok(item.clone()),
            _ => Err(wrong_args(&usage_of(usage, "list"))),
        },
        Builtin::SlotOp(op) => slot_op(interp, vm, obj, op, usage, args),
        // `Slot_Unknown`: a word that is not an option is an item for the
        // default operation; an unknown option goes on down the chain.
        Builtin::SlotUnknown => match args.first().map(to_tcl_string) {
            Some(first) if first.starts_with('-') => {
                if index + 1 < chain.calls.len() {
                    invoke_context(
                        interp,
                        vm,
                        obj,
                        Arc::clone(chain),
                        index + 1,
                        args,
                        usage,
                        ns,
                    )
                } else {
                    Err(unknown_method_named(&first))
                }
            }
            _ => slot_call(interp, vm, obj, "--default-operation", args),
        },
        Builtin::SlotGet(which, class) => slot_get(interp, which, class, usage, args),
        Builtin::SlotSet(which, class) => slot_set(interp, which, class, usage, args),
        Builtin::SlotResolveClass => slot_resolve_class(interp, usage, args),
        Builtin::Configure => configure(interp, vm, obj, usage, args),
        // `TclOO_Configurable_Constructor`: the new class mixes in
        // `oo::configuresupport::configurable`, then the class constructor runs.
        Builtin::ConfigurableCtor => {
            if args.len() > 1 {
                return Err(wrong_args(&usage_of(usage, "?definitionScript?")));
            }
            {
                let mut state = lock(interp);
                let support = "::oo::configuresupport::configurable".to_string();
                set_mixins(&mut state.oo, obj, true, &[support], "::")?;
            }
            if index + 1 < chain.calls.len() {
                invoke_context(
                    interp,
                    vm,
                    obj,
                    Arc::clone(chain),
                    index + 1,
                    args,
                    usage,
                    ns,
                )
            } else {
                Ok(val(""))
            }
        }
        // `TclOO_Class_Cloned`: the rest of the chain, then
        // `UpdateClassDelegatesAfterClone` gives the copy a copy of the
        // origin's delegate in place of the origin's own, so the two classes'
        // class methods evolve apart.
        Builtin::ClassCloned => {
            let [origin] = args else {
                return Err(wrong_args(&usage_of(usage, "originObject")));
            };
            let origin_name = to_tcl_string(origin);
            let origin = lock(interp)
                .oo
                .lookup(&origin_name, ns)
                .ok_or_else(|| not_an_object(&origin_name))?;
            if index + 1 < chain.calls.len() {
                invoke_context(
                    interp,
                    vm,
                    obj,
                    Arc::clone(chain),
                    index + 1,
                    args,
                    usage,
                    ns,
                )?;
            }
            let (origin_delegate, target_name) = {
                let state = lock(interp);
                let oo = &state.oo;
                if !oo.alive(origin) || !oo.is_class(origin) || !oo.is_class(obj) {
                    return Ok(val(""));
                }
                let Some(d) = oo.delegate_of(origin) else {
                    return Ok(val(""));
                };
                (d, format!("{}:: oo ::delegate", oo.ns_of(obj)))
            };
            let source = lock(interp).oo.name(origin_delegate);
            copy_cmd(interp, vm, "::", &[val(source), val(target_name.clone())])?;
            let mut state = lock(interp);
            let oo = &mut state.oo;
            let Some(&copy) = oo.by_command.get(store_key(&target_name)) else {
                return Ok(val(""));
            };
            if let Some(at) = oo
                .obj(obj)
                .mixins
                .iter()
                .position(|&m| m == origin_delegate)
            {
                oo.class_mut(origin_delegate)
                    .instances
                    .retain(|&i| i != obj);
                oo.obj_mut(obj).mixins[at] = copy;
                oo.class_mut(copy).instances.push(obj);
            }
            Ok(val(""))
        }
        Builtin::ClassCtor => {
            if args.len() > 1 {
                return Err(wrong_args(&usage_of(usage, "?definitionScript?")));
            }
            // `TclOO_Class_Constructor` makes the class's delegate
            // (`<class>:: oo ::delegate`), an instance of `oo::class` whose
            // constructor does not run, before the definition script runs —
            // it takes the next object number, which is why classes are
            // numbered in twos — and mixes it in afterwards, whatever the
            // script did (`PostClassConstructor`).
            let delegate = {
                let mut state = lock(interp);
                let name = format!("{}:: oo ::delegate", state.oo.ns_of(obj));
                let crate::runtime::State { oo, ns: reg, .. } = &mut *state;
                let class_cls = oo.class_cls;
                oo.alloc(reg, class_cls, Some(name), None)
            };
            let result = match args {
                [script] => {
                    let script = to_tcl_string(script);
                    run_definition(interp, vm, obj, true, false, ns, "class", &script)
                        .map(|_| val(""))
                }
                _ => Ok(val("")),
            };
            lock(interp).oo.mixin_class_delegates(obj, delegate);
            result
        }
    }
}

/// `oo::object`'s `variable` method reached at run time — `$obj variable x`
/// from outside a method, or `my variable` with a computed name. Linking into
/// the caller's frame is only possible here at the global level, where the
/// link is an alias of two global names.
fn link_variables(
    interp: &Shared,
    vm: &mut VM,
    obj: Id,
    args: &[Value],
) -> Result<Value, TclError> {
    let ns = lock(interp).oo.ns_of(obj);
    for name in strings(args) {
        if name.contains("::") {
            return Err(coded_err(
                format!("variable name \"{name}\" illegal: must not contain namespace separator"),
                &["TCL", "UPVAR", "INVERTED"],
            ));
        }
        if !crate::runtime::levels(vm).is_empty() {
            return Err(TclError::plain(format!(
                "linking the object variable \"{name}\" into a procedure's frame at run time is \
                 not supported yet: write \"my variable {name}\" with the name spelt out"
            )));
        }
        crate::runtime::flush_globals(vm, interp);
        crate::runtime::alias_global(interp, &name, &format!("{ns}::{name}"))
            .map_err(TclError::plain)?;
        crate::runtime::reseed_globals(vm, interp);
    }
    Ok(val(""))
}

/// `TclOO_Object_Unknown`: the error for a method nothing implements.
fn unknown_method(
    interp: &Shared,
    obj: Id,
    chain: &Chain,
    args: &[Value],
    usage: &str,
) -> Result<Value, TclError> {
    let Some(method) = args.first().map(to_tcl_string) else {
        return Err(wrong_args(&usage_of(usage, "method ?arg ...?")));
    };
    let state = lock(interp);
    let oo = &state.oo;
    // A caller inside a method may see its declarer's private methods.
    let (mut cobj, mut ccls) = (None, None);
    // A built-in method runs in its caller's frame, so the innermost frame is
    // the caller's.
    if let Some(Frame::Method {
        chain: c, index, ..
    }) = oo.stack.last()
    {
        match c.calls[*index].declarer {
            Declarer::Object(o) if o == obj => cobj = Some(o),
            Declarer::Class(k) if oo.alive(k) && oo.reachable(k, oo.obj(obj).class) => {
                ccls = Some(k)
            }
            _ => {}
        }
    }
    let public = chain.flags & PUBLIC_METHOD != 0;
    let names = oo.sorted_methods(obj, cobj, ccls, if public { PUBLIC_METHOD } else { 0 });
    let msg = match names.as_slice() {
        [] => format!(
            "object \"{}\" has no {}",
            oo.name(obj),
            if public { "visible methods" } else { "methods" }
        ),
        [only] => format!("unknown method \"{method}\": must be {only}"),
        [init @ .., last] => format!(
            "unknown method \"{method}\": must be {} or {last}",
            init.join(", ")
        ),
    };
    Err(coded_err(msg, &["TCL", "LOOKUP", "METHOD", &method]))
}

const IN_LIST: u8 = 1;
const NO_IMPLEMENTATION: u8 = 2;

/// A method name and whether it is wanted in a sorted method list.
type NameTable = BTreeMap<String, u8>;

/// `AddStandardMethodName`: the first record seen for a name decides whether
/// it is listed; a later one with an implementation can only supply the
/// implementation a flags-only record lacked.
fn add_standard_name(names: &mut NameTable, name: &str, m: &Method, flags: u32) {
    if m.flags & TRUE_PRIVATE_METHOD != 0 {
        return;
    }
    match names.get_mut(name) {
        None => {
            let mut wanted = if flags & PUBLIC_METHOD == 0 || m.flags & PUBLIC_METHOD != 0 {
                IN_LIST
            } else {
                0
            };
            if m.imp.is_none() {
                wanted |= NO_IMPLEMENTATION;
            }
            names.insert(name.to_string(), wanted);
        }
        Some(w) if *w & NO_IMPLEMENTATION != 0 && m.imp.is_some() => *w &= !NO_IMPLEMENTATION,
        Some(_) => {}
    }
}

/// `AddPrivateMethodNames`.
fn add_private_names(names: &mut NameTable, table: &BTreeMap<String, Method>) {
    for (n, m) in table {
        if m.flags & TRUE_PRIVATE_METHOD != 0 {
            names.insert(n.clone(), IN_LIST);
        }
    }
}

/// `SortMethodNames`: the wanted names, in byte order.
fn wanted_names(names: NameTable, flags: u32) -> Vec<String> {
    let want_public = flags & PUBLIC_METHOD != 0;
    names
        .into_iter()
        .filter(|(_, w)| (!want_public || w & IN_LIST != 0) && w & NO_IMPLEMENTATION == 0)
        .map(|(n, _)| n)
        .collect()
}

impl Oo {
    /// `TclOOGetSortedMethodList`.
    fn sorted_methods(
        &self,
        obj: Id,
        cobj: Option<Id>,
        ccls: Option<Id>,
        flags: u32,
    ) -> Vec<String> {
        let mut names = NameTable::new();
        let o = self.obj(obj);
        for (n, m) in &o.methods {
            add_standard_name(&mut names, n, m, flags);
        }
        // `WANT_UNEXPORTED`: true unless asking for Itcl or TIP 500 privates.
        if flags & (0x02 | TRUE_PRIVATE_METHOD) == 0 {
            for (n, m) in &self.class(o.class).methods {
                if m.flags & (PUBLIC_METHOD | TRUE_PRIVATE_METHOD) == 0 {
                    add_standard_name(&mut names, n, m, flags);
                }
            }
        }
        if let Some(co) = cobj {
            add_private_names(&mut names, &self.obj(co).methods);
        }
        if let Some(cc) = ccls {
            add_private_names(&mut names, &self.class(cc).methods);
        }
        let mut examined = HashSet::new();
        self.add_class_method_names(o.class, flags, &mut names, &mut examined);
        for &mixin in &o.mixins {
            self.add_class_method_names(mixin, flags | TRAVERSED_MIXIN, &mut names, &mut examined);
        }
        wanted_names(names, flags)
    }

    /// `AddClassMethodNames`.
    fn add_class_method_names(
        &self,
        mut cls: Id,
        flags: u32,
        names: &mut NameTable,
        examined: &mut HashSet<Id>,
    ) {
        loop {
            if !examined.insert(cls) {
                return;
            }
            let c = self.class(cls);
            for &mixin in &c.mixins {
                if mixin != cls {
                    self.add_class_method_names(mixin, flags | TRAVERSED_MIXIN, names, examined);
                }
            }
            for (n, m) in &c.methods {
                add_standard_name(names, n, m, flags);
            }
            match c.superclasses.as_slice() {
                [only] => cls = *only,
                many => {
                    for &s in many {
                        self.add_class_method_names(s, flags, names, examined);
                    }
                    return;
                }
            }
        }
    }

    /// `TclOOGetSortedClassMethodList`.
    fn sorted_class_methods(&self, cls: Id, flags: u32) -> Vec<String> {
        let mut names = NameTable::new();
        let mut examined = HashSet::new();
        self.add_class_method_names(cls, flags, &mut names, &mut examined);
        if flags & TRUE_PRIVATE_METHOD != 0 {
            add_private_names(&mut names, &self.class(cls).methods);
        }
        wanted_names(names, flags & !TRUE_PRIVATE_METHOD)
    }
}

// ── creating and destroying objects ──────────────────────────────────────

/// Whether a command named `fqn` already exists, which an object may not be
/// created over.
fn command_exists(state: &crate::runtime::State, fqn: &str) -> bool {
    state.ns.command(fqn).is_some()
        || state.commands.contains_key(store_key(fqn))
        || (parent_of(fqn) == "::" && crate::names::is_command(tail(fqn)))
}

/// `TclNRNewObjectInstance`: make an instance of `class` and run its
/// constructors. Answers the new object's name.
#[allow(clippy::too_many_arguments)]
fn new_instance(
    interp: &Shared,
    vm: &mut VM,
    class: Id,
    name: Option<&str>,
    ns_name: Option<&str>,
    ctor_args: &[Value],
    usage: &str,
    caller_ns: &str,
) -> Result<Value, TclError> {
    let id = {
        let mut state = lock(interp);
        let command = name.map(|n| resolve(caller_ns, n));
        if let (Some(fqn), Some(written)) = (&command, name) {
            if command_exists(&state, fqn) {
                return Err(oo_err(
                    format!(
                        "can't create object \"{written}\": command already exists with that name"
                    ),
                    "OVERWRITE_OBJECT",
                ));
            }
        }
        let ns = ns_name.map(|n| resolve(caller_ns, n));
        if let Some(ns) = &ns {
            if state.ns.exists(ns) {
                return Err(TclError::plain(format!(
                    "can't create namespace \"{ns}\": already exists"
                )));
            }
        }
        let crate::runtime::State { oo, ns: reg, .. } = &mut *state;
        oo.alloc(reg, class, command, ns)
    };
    let chain = lock(interp).oo.chain_for(id, None, CONSTRUCTOR, None, None);
    if let Some(chain) = chain {
        let outcome = invoke_context(
            interp,
            vm,
            id,
            Arc::new(chain),
            0,
            ctor_args,
            usage,
            caller_ns,
        );
        let deleted = {
            let state = lock(interp);
            !state.oo.alive(id) || state.oo.obj(id).destructing
        };
        match outcome {
            Err(e) => {
                if !deleted {
                    let _ = destroy_object(interp, vm, id);
                }
                return Err(e);
            }
            Ok(_) if deleted => {
                return Err(oo_err("object deleted in constructor", "STILLBORN"));
            }
            Ok(_) => {}
        }
    }
    Ok(val(lock(interp).oo.name(id)))
}

/// `TclOO_Object_Destroy`: run the destructors, once, then delete the object.
/// The destructor's error, if any, is the answer — after the object is gone.
fn destroy_object(interp: &Shared, vm: &mut VM, id: Id) -> Result<(), TclError> {
    let chain = {
        let mut state = lock(interp);
        if !state.oo.alive(id) || state.oo.obj(id).destructing {
            return Ok(());
        }
        state.oo.obj_mut(id).destructing = true;
        state.oo.chain_for(id, None, DESTRUCTOR, None, None)
    };
    let outcome = match chain {
        Some(chain) => {
            invoke_context(interp, vm, id, Arc::new(chain), 0, &[], "", "::").map(|_| ())
        }
        None => Ok(()),
    };
    delete_object(interp, vm, id);
    outcome
}

/// `ObjectNamespaceDeleted`: take the object out of every structure that knows
/// it. A class takes its subclasses and its instances with it, last first, as
/// `TclOODeleteDescendants` does.
fn delete_object(interp: &Shared, vm: &mut VM, id: Id) {
    let descendants: Vec<Id> = {
        let mut state = lock(interp);
        if !state.oo.alive(id) || !state.oo.deleting.insert(id) {
            return;
        }
        match &state.oo.obj(id).class_data {
            // The classes it is mixed into, its subclasses, then its instances —
            // which include the objects it is mixed into.
            Some(c) => c
                .mixin_users
                .iter()
                .rev()
                .chain(c.subclasses.iter().rev())
                .chain(c.instances.iter().rev())
                .copied()
                .filter(|&d| d != id)
                .filter(|d| !state.oo.deleting.contains(d))
                .collect(),
            None => Vec::new(),
        }
    };
    for d in descendants {
        if d == 0 || d == 1 {
            continue;
        }
        let gone = {
            let state = lock(interp);
            !state.oo.alive(d)
        };
        if gone {
            continue;
        }
        // Destructor errors here have nowhere to go, as in tclsh.
        let _ = destroy_object(interp, vm, d);
        delete_object(interp, vm, d);
    }
    // Deleting the namespace deletes the commands in it, and an object whose
    // command is one of them goes too (`[self class] create child` inside a
    // method names the child in the creator's namespace).
    let nested: Vec<Id> = {
        let state = lock(interp);
        let prefix = format!("{}::", store_key(&state.oo.ns_of(id)));
        let mut ids: Vec<Id> = state
            .oo
            .by_command
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, &i)| i)
            .collect();
        ids.sort_unstable();
        ids
    };
    for d in nested {
        let _ = destroy_object(interp, vm, d);
        delete_object(interp, vm, d);
    }
    let mut state = lock(interp);
    let crate::runtime::State {
        oo,
        ns: reg,
        globals,
        ..
    } = &mut *state;
    if !oo.alive(id) {
        oo.deleting.remove(&id);
        return;
    }
    let o = oo.objects[id].take().expect("a live object");
    oo.deleting.remove(&id);
    oo.graveyard.insert(id, (o.command.clone(), o.ns.clone()));
    // Out of the class's instances, the superclasses' subclasses and the
    // mixins' users.
    if oo.alive(o.class) {
        if let Some(c) = oo.obj_mut(o.class).class_data.as_mut() {
            c.instances.retain(|&i| i != id);
        }
    }
    for &m in &o.mixins {
        if oo.alive(m) {
            if let Some(c) = oo.obj_mut(m).class_data.as_mut() {
                c.instances.retain(|&i| i != id);
            }
        }
    }
    if let Some(c) = &o.class_data {
        for &s in &c.superclasses {
            if oo.alive(s) {
                if let Some(sc) = oo.obj_mut(s).class_data.as_mut() {
                    sc.subclasses.retain(|&i| i != id);
                }
            }
        }
        for &m in &c.mixins {
            if oo.alive(m) {
                if let Some(mc) = oo.obj_mut(m).class_data.as_mut() {
                    mc.mixin_users.retain(|&i| i != id);
                }
            }
        }
        for &u in &c.mixin_users {
            if oo.alive(u) {
                let user = oo.obj_mut(u);
                user.mixins.retain(|&i| i != id);
                if let Some(uc) = user.class_data.as_mut() {
                    uc.mixins.retain(|&i| i != id);
                }
            }
        }
    }
    oo.by_command.remove(store_key(&o.command));
    oo.by_ns.remove(&o.ns);
    reg.forget_command(&o.command);
    reg.forget_namespace(&o.ns);
    // `link`'s commands go with the object (`linkedCmdsList`).
    let gone: Vec<String> = oo
        .links
        .iter()
        .filter(|(_, (owner, _))| *owner == id)
        .map(|(k, _)| k.clone())
        .collect();
    for key in gone {
        oo.links.remove(&key);
        reg.forget_command(&format!("::{key}"));
    }
    let prefix = format!("{}::", store_key(&o.ns));
    globals.retain(|k, _| !k.starts_with(&prefix));
}

/// `rename obj {}`: the object is destroyed, its destructor's error dropped.
pub(crate) fn rename_away(interp: &Shared, vm: &mut VM, fqn: &str) -> bool {
    let id = lock(interp).oo.by_command.get(store_key(fqn)).copied();
    match id {
        Some(id) => {
            let _ = destroy_object(interp, vm, id);
            true
        }
        None => false,
    }
}

/// `rename obj new`: the object's command moves.
pub(crate) fn renamed(state: &mut crate::runtime::State, old: &str, new: &str) {
    let oo = &mut state.oo;
    if let Some(id) = oo.by_command.remove(store_key(old)) {
        oo.obj_mut(id).command = new.to_string();
        oo.by_command.insert(store_key(new).to_string(), id);
    }
}

/// Whether `fqn` names an object's command.
pub(crate) fn is_object(state: &crate::runtime::State, fqn: &str) -> bool {
    state.oo.by_command.contains_key(store_key(fqn))
}

// ── self, next, nextto ───────────────────────────────────────────────────

fn current_frame(interp: &Shared, written: &str) -> Result<(Id, Arc<Chain>, usize), TclError> {
    match lock(interp).oo.stack.last() {
        Some(Frame::Method { obj, chain, index }) => Ok((*obj, Arc::clone(chain), *index)),
        _ => Err(oo_err(
            format!("{written} may only be called from inside a method"),
            "CONTEXT_REQUIRED",
        )),
    }
}

fn chain_kind(chain: &Chain) -> &'static str {
    if chain.flags & CONSTRUCTOR != 0 {
        "constructor"
    } else if chain.flags & DESTRUCTOR != 0 {
        "destructor"
    } else {
        "method"
    }
}

const SELF_SUBCOMMANDS: &[&str] = &[
    "call",
    "caller",
    "class",
    "filter",
    "method",
    "namespace",
    "next",
    "object",
    "target",
];

/// `TclOOSelfObjCmd`.
fn self_cmd(interp: &Shared, written: &str, args: &[Value]) -> Result<Value, TclError> {
    let (obj, chain, index) = current_frame(interp, written)?;
    let sub = match args {
        [] => "object",
        [sub] => index_of("subcommand", &to_tcl_string(sub), SELF_SUBCOMMANDS)?,
        _ => return Err(wrong_args(&format!("{written} subcommand"))),
    };
    let state = lock(interp);
    let oo = &state.oo;
    let current = &chain.calls[index];
    let declared_name = |c: &Call| -> String {
        if chain.flags & CONSTRUCTOR != 0 {
            "<constructor>".to_string()
        } else if chain.flags & DESTRUCTOR != 0 {
            "<destructor>".to_string()
        } else {
            c.name.clone()
        }
    };
    let answer = match sub {
        "object" => oo.name(obj),
        "namespace" => oo.ns_of(obj),
        "class" => match current.declarer {
            Declarer::Class(c) => oo.name(c),
            Declarer::Object(_) => {
                return Err(oo_err("method not defined by a class", "UNMATCHED_CONTEXT"));
            }
        },
        "method" => declared_name(current),
        "filter" => {
            if !current.filter {
                return Err(oo_err(
                    "not inside a filtering context",
                    "UNMATCHED_CONTEXT",
                ));
            }
            let (who, kind) = match current.filter_declarer {
                Some(c) => (oo.name(c), "class"),
                None => (oo.name(obj), "object"),
            };
            crate::list::join(&[who, kind.to_string(), current.name.clone()])
        }
        "target" => {
            if !current.filter {
                return Err(oo_err(
                    "not inside a filtering context",
                    "UNMATCHED_CONTEXT",
                ));
            }
            let target = chain.calls[index..]
                .iter()
                .find(|c| !c.filter)
                .expect("a filtering chain ends in a method");
            crate::list::join(&[oo.declarer_name(target.declarer), target.name.clone()])
        }
        "next" => match chain.calls.get(index + 1) {
            Some(next) => {
                crate::list::join(&[oo.declarer_name(next.declarer), declared_name(next)])
            }
            None => String::new(),
        },
        "caller" => {
            // The method frame this invocation was called from.
            let below = oo.stack.iter().rev().skip(1).find_map(|f| match f {
                Frame::Method { obj, chain, index } => Some((*obj, chain, *index)),
                Frame::Define { .. } => None,
            });
            match below {
                Some((cobj, cchain, cindex)) => {
                    let call = &cchain.calls[cindex];
                    let name = if cchain.flags & CONSTRUCTOR != 0 {
                        "<constructor>".to_string()
                    } else if cchain.flags & DESTRUCTOR != 0 {
                        "<destructor>".to_string()
                    } else {
                        call.name.clone()
                    };
                    crate::list::join(&[oo.declarer_name(call.declarer), oo.name(cobj), name])
                }
                None => return Err(oo_err("caller is not an object", "CONTEXT_REQUIRED")),
            }
        }
        "call" => crate::list::join(&[oo.render_chain(&chain), index.to_string()]),
        _ => unreachable!("index_of answers a table entry"),
    };
    Ok(val(answer))
}

/// The `oo::Helpers` commands `callback` / `mymethod` (`TclOOCallbackObjCmd`),
/// `classvariable` (`TclOOClassVariableObjCmd`) and `link`
/// (`TclOOLinkObjCmd`), and the check a compiled `classvariable` runs.
fn helper(interp: &Shared, which: &str, written: &str, args: &[Value]) -> Result<Value, TclError> {
    let words = strings(args);
    let classvariable = which.starts_with("classvariable");
    if classvariable && words.is_empty() {
        return Err(wrong_args(&format!("{written} name ...")));
    }
    let mut state = lock(interp);
    let in_method = matches!(state.oo.stack.last(), Some(Frame::Method { .. }));
    let (obj, declarer) = match state.oo.current_method() {
        Some((obj, chain, index)) if in_method => (obj, chain.calls[index].declarer),
        _ => {
            return Err(oo_err(
                format!("{written} may only be called from inside a method"),
                "CONTEXT_REQUIRED",
            ))
        }
    };
    let obj_ns = state.oo.ns_of(obj);
    match which {
        "callback" | "mymethod" => {
            if words.is_empty() {
                return Err(wrong_args(&format!("{written} method ...")));
            }
            let mut list = vec![format!("{obj_ns}::my")];
            list.extend(words);
            Ok(val(crate::list::join(&list)))
        }
        "link" => {
            let mut made = Vec::new();
            for word in &words {
                let parts = crate::list::split(word).map_err(TclError::plain)?;
                let (src, dst) = match parts.as_slice() {
                    [one] => (one.clone(), one.clone()),
                    [src, dst] => (src.clone(), dst.clone()),
                    _ => {
                        return Err(oo_err(
                            "bad link description; must only have one or two elements",
                            "CMDLINK_FORMAT",
                        ))
                    }
                };
                let src = if src.starts_with("::") {
                    src
                } else {
                    format!("{obj_ns}::{src}")
                };
                made.push((src, dst));
            }
            let crate::runtime::State { oo, ns: reg, .. } = &mut *state;
            for (src, dst) in made {
                reg.define(&src);
                oo.links.insert(store_key(&src).to_string(), (obj, dst));
            }
            Ok(val(""))
        }
        _ => {
            let Declarer::Class(_) = declarer else {
                return Err(oo_err("method not defined by a class", "UNMATCHED_CONTEXT"));
            };
            for name in &words {
                let what = if crate::assoc::string_match(name, "*(*)") {
                    "scalar variable that looks like an array element"
                } else if name.contains("::") {
                    "local variable with a namespace separator in it"
                } else {
                    continue;
                };
                return Err(coded_err(
                    format!("bad variable name \"{name}\": can't create a {what}"),
                    &["TCL", "UPVAR", "LOCAL_ELEMENT"],
                ));
            }
            if which == "classvariable" {
                // Reached at run time only when a name was computed: the link
                // is to a local slot, which only the compiler can address.
                return Err(TclError::plain(format!(
                    "linking the class variable \"{}\" into a method's frame at run time is not \
                     supported yet: write \"classvariable {}\" with the name spelt out",
                    words[0], words[0]
                )));
            }
            Ok(val(""))
        }
    }
}

/// `TclOONextObjCmd`: the next implementation in the chain, called with the
/// arguments given.
fn next_cmd(
    interp: &Shared,
    vm: &mut VM,
    written: &str,
    ns: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let (obj, chain, index) = current_frame(interp, written)?;
    if index + 1 >= chain.calls.len() {
        return Err(oo_err(
            format!("no next {} implementation", chain_kind(&chain)),
            "NOTHING_NEXT",
        ));
    }
    invoke_context(interp, vm, obj, chain, index + 1, args, written, ns)
}

/// `TclOONextToObjCmd`: skip ahead to the implementation `class` declares.
fn nextto_cmd(
    interp: &Shared,
    vm: &mut VM,
    written: &str,
    ns: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let (obj, chain, index) = current_frame(interp, written)?;
    let Some(class_word) = args.first().map(to_tcl_string) else {
        return Err(wrong_args(&format!("{written} class ?arg...?")));
    };
    let class = {
        let state = lock(interp);
        let id = state
            .oo
            .lookup(&class_word, ns)
            .ok_or_else(|| not_an_object(&class_word))?;
        if !state.oo.is_class(id) {
            return Err(oo_err(
                format!("\"{class_word}\" is not a class"),
                "CLASS_REQUIRED",
            ));
        }
        id
    };
    let declared_by = |c: &Call| !c.filter && c.declarer == Declarer::Class(class);
    if let Some(i) = (index + 1..chain.calls.len()).find(|&i| declared_by(&chain.calls[i])) {
        let usage = format!("{written} {class_word}");
        return invoke_context(interp, vm, obj, chain, i, &args[1..], &usage, ns);
    }
    let kind = chain_kind(&chain);
    if chain.calls[..=index].iter().any(declared_by) {
        return Err(oo_err(
            format!("{kind} implementation by \"{class_word}\" not reachable from here"),
            "CLASS_NOT_REACHABLE",
        ));
    }
    Err(oo_err(
        format!("{kind} has no non-filter implementation by \"{class_word}\""),
        "CLASS_NOT_THERE",
    ))
}

// ── oo::define and oo::objdefine ─────────────────────────────────────────

/// `TclOODefineObjCmd` and `TclOOObjDefObjCmd`.
fn define_cmd(
    interp: &Shared,
    vm: &mut VM,
    class: bool,
    written: &str,
    ns: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let words = strings(args);
    if words.len() < 2 {
        let rest = if class {
            "className arg ?arg ...?"
        } else {
            "objectName arg ?arg ...?"
        };
        return Err(wrong_args(&format!("{written} {rest}")));
    }
    let id = {
        let state = lock(interp);
        let id = state
            .oo
            .lookup(&words[0], ns)
            .ok_or_else(|| not_an_object(&words[0]))?;
        if class && !state.oo.is_class(id) {
            return Err(coded_err(
                format!("{} does not refer to a class", words[0]),
                &["TCL", "LOOKUP", "CLASS", &words[0]],
            ));
        }
        id
    };
    if words.len() == 2 {
        let kind = if class { "class" } else { "object" };
        return run_definition(interp, vm, id, class, false, ns, kind, &words[1]);
    }
    let space = definition_space(interp, id, class)?;
    let frame = Frame::Define {
        obj: id,
        class,
        private: false,
        outer: ns.to_string(),
        space: space.clone(),
    };
    // `MagicDefinitionInvoke`: the word is found in the definition namespace
    // by exact name or unique prefix (`FindCommand`); a word that names no
    // command there is run as written, inside the definition context.
    if let Some(cmd) = define_lookup(&space, &words[1]) {
        let usage = format!("{written} {} {}", words[0], words[1]);
        return with_frame(interp, frame, || {
            define_sub(interp, vm, cmd, &usage, &words[2..])
        });
    }
    let mut call = args[1..].to_vec();
    if let Some(full) = find_in_namespace(interp, &space, &words[1]) {
        call[0] = val(full);
    }
    with_frame(interp, frame, || {
        crate::procs::call_words_sync(interp, vm, &call, Some(&space))
    })
}

/// The namespace a definition of `obj` runs in, or `InitDefineContext`'s
/// refusal when no definition namespace along its classes exists.
fn definition_space(interp: &Shared, obj: Id, class: bool) -> Result<String, TclError> {
    let state = lock(interp);
    state
        .oo
        .define_space(&state.ns, obj, class)
        .ok_or_else(|| oo_err("no definition namespace available", "MONKEY_BUSINESS"))
}

/// `property` and `properties`, the commands of the configurable definition
/// namespaces.
const CFG_CMDS: &[&str] = &["properties", "property"];

/// The commands of a definition namespace this module implements:
/// `::oo::define`'s and `::oo::objdefine`'s, and those of the configurable
/// definition namespaces. Any other definition namespace holds only what the
/// script put in it.
fn define_table(space: &str) -> Option<&'static [&'static str]> {
    match space {
        "::oo::define" => Some(DEFINE_CMDS),
        "::oo::objdefine" => Some(OBJDEFINE_CMDS),
        CFG_CLASS_NS | CFG_OBJECT_NS => Some(CFG_CMDS),
        _ => None,
    }
}

/// The definition command `word` names in `space`. `::oo::define` and
/// `::oo::objdefine` take a unique prefix too (their unknown handler is
/// `oo::UnknownDefinition`); the configurable namespaces take the exact name
/// of one of their own commands or, through their namespace path, of one of
/// `oo::define`'s or `oo::objdefine`'s.
fn define_lookup(space: &str, word: &str) -> Option<&'static str> {
    let exact = |table: &[&'static str]| table.iter().find(|c| **c == word).copied();
    match space {
        "::oo::define" => define_prefix(word, DEFINE_CMDS),
        "::oo::objdefine" => define_prefix(word, OBJDEFINE_CMDS),
        CFG_CLASS_NS => exact(CFG_CMDS).or_else(|| exact(DEFINE_CMDS)),
        CFG_OBJECT_NS => exact(CFG_CMDS).or_else(|| exact(OBJDEFINE_CMDS)),
        _ => None,
    }
}

/// `FindCommand`: the command of namespace `space` named `word` exactly, or
/// the one it is a unique prefix of — never a name with `::` in it.
fn find_in_namespace(interp: &Shared, space: &str, word: &str) -> Option<String> {
    if word.is_empty() || word.contains("::") {
        return None;
    }
    let names: Vec<String> = lock(interp)
        .ns
        .commands_in(space)
        .iter()
        .map(|fqn| tail(fqn).to_string())
        .collect();
    if names.iter().any(|n| n == word) {
        return Some(format!("{space}::{word}"));
    }
    let mut hits = names.iter().filter(|n| n.starts_with(word));
    match (hits.next(), hits.next()) {
        (Some(one), None) => Some(format!("{space}::{one}")),
        _ => None,
    }
}

/// `TclOOUnknownDefinition`: the command of the definition namespace that the
/// word is a unique prefix of, run with the rest of the words.
fn unknown_definition(interp: &Shared, vm: &mut VM, args: &[Value]) -> Result<Value, TclError> {
    let words = strings(args);
    let Some(sought) = words.first() else {
        return Err(oo_err("bad call of unknown handler", "BAD_UNKNOWN"));
    };
    let space =
        match lock(interp).oo.stack.last() {
            Some(Frame::Define { space, .. }) => space.clone(),
            _ => return Err(oo_err(
                "this command may only be called from within the context of an ::oo::define or \
                 ::oo::objdefine command",
                "MONKEY_BUSINESS",
            )),
        };
    let names: Vec<String> = match define_table(&space) {
        Some(table) => table.iter().map(|s| s.to_string()).collect(),
        None => lock(interp)
            .ns
            .commands_in(&space)
            .iter()
            .map(|fqn| tail(fqn).to_string())
            .collect(),
    };
    let mut hits = names
        .iter()
        .filter(|n| !sought.is_empty() && n.starts_with(sought.as_str()));
    let matched = match (hits.next(), hits.next()) {
        (Some(one), None) => one.clone(),
        _ => {
            return Err(coded_err(
                format!("invalid command name \"{sought}\""),
                &["TCL", "LOOKUP", "COMMAND", sought],
            ))
        }
    };
    match define_lookup(&space, &matched) {
        Some(cmd) => define_sub(interp, vm, cmd, &matched, &words[1..]),
        None => {
            let mut call = vec![val(format!("{space}::{matched}"))];
            call.extend_from_slice(&args[1..]);
            crate::procs::call_words_sync(interp, vm, &call, Some(&space))
        }
    }
}

/// Evaluate a definition script: in `::oo::define` (or `::oo::objdefine`),
/// whose commands are the definition commands, with the object being defined
/// as the innermost definition context.
#[allow(clippy::too_many_arguments)]
fn run_definition(
    interp: &Shared,
    vm: &mut VM,
    id: Id,
    class: bool,
    private: bool,
    outer: &str,
    kind: &str,
    script: &str,
) -> Result<Value, TclError> {
    let space = definition_space(interp, id, class)?;
    let src = format!(
        "namespace eval {} {}",
        crate::list::quote(&space, false),
        crate::list::quote(script, false)
    );
    let frame = Frame::Define {
        obj: id,
        class,
        private,
        outer: outer.to_string(),
        space: space.clone(),
    };
    let saved = lock(interp).oo.name(id);
    crate::runtime::with_written_back(interp, vm, |interp| {
        with_frame(interp, frame, || crate::runtime::run_source(interp, &src))
    })
    .map_err(|e| {
        // `GenerateErrorInfo`: the object's name now, which a script may have
        // renamed, or the one it had if the script deleted it — cut at
        // `OBJNAME_LENGTH_IN_ERRORINFO_LIMIT` characters.
        let name = {
            let state = lock(interp);
            if state.oo.alive(id) {
                state.oo.name(id)
            } else {
                saved.clone()
            }
        };
        let name = match name.char_indices().nth(OBJNAME_LIMIT) {
            Some((at, _)) => format!("{}...", &name[..at]),
            None => name,
        };
        relabel(
            e,
            &format!("namespace eval {} ", crate::list::quote(&space, false)),
            |line| format!("(in definition script for {kind} \"{name}\" line {line})"),
        )
    })
}

/// A procedure-like method from an argument list and a body.
fn proc_method(params: &str, body: &str) -> Result<Arc<Imp>, TclError> {
    let sig = crate::procs::parse_signature(params).map_err(TclError::plain)?;
    Ok(Arc::new(Imp::Proc(Box::new(ProcImp {
        params: params.to_string(),
        body: body.to_string(),
        sig,
        compiled: Mutex::new(None),
    }))))
}

/// The method table a definition command edits: the class's in `oo::define`,
/// the object's own in `oo::objdefine`.
fn table_of(oo: &mut Oo, obj: Id, class: bool) -> &mut BTreeMap<String, Method> {
    if class {
        &mut oo.class_mut(obj).methods
    } else {
        &mut oo.obj_mut(obj).methods
    }
}

/// `OBJNAME_LENGTH_IN_ERRORINFO_LIMIT`: how much of an object's name a
/// definition script's error context repeats.
const OBJNAME_LIMIT: usize = 30;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Slot {
    Superclass,
    Mixin,
    Filter,
    Variable,
    /// `readableproperties` and `objreadableproperties`.
    ReadableProps,
    /// `writableproperties` and `objwritableproperties`.
    WritableProps,
}

/// One definition command, run against the innermost definition context.
/// `written` is what a `wrong # args` repeats in front of the usage.
fn define_sub(
    interp: &Shared,
    vm: &mut VM,
    cmd: &str,
    written: &str,
    args: &[String],
) -> Result<Value, TclError> {
    let (obj, class, private, outer, space) =
        match lock(interp).oo.stack.last() {
            Some(Frame::Define {
                obj,
                class,
                private,
                outer,
                space,
            }) => (*obj, *class, *private, outer.clone(), space.clone()),
            _ => return Err(oo_err(
                "this command may only be called from within the context of an ::oo::define or \
                 ::oo::objdefine command",
                "MONKEY_BUSINESS",
            )),
        };
    if !lock(interp).oo.alive(obj) {
        return Err(oo_err(
            "this command cannot be called when the object has been deleted",
            "MONKEY_BUSINESS",
        ));
    }
    // Class names inside a definition resolve from where it was invoked.
    let ns = outer.as_str();
    let empty = Ok(val(""));
    match cmd {
        "constructor" | "destructor" => {
            let (params, body) = match (cmd, args) {
                ("constructor", [params, body]) => (params.as_str(), body.as_str()),
                ("destructor", [body]) => ("", body.as_str()),
                ("constructor", _) => return Err(wrong_args(&usage_of(written, "arguments body"))),
                _ => return Err(wrong_args(&usage_of(written, "body"))),
            };
            let method = if body.is_empty() {
                None
            } else {
                Some(Method {
                    flags: PUBLIC_METHOD,
                    imp: Some(proc_method(params, body)?),
                })
            };
            let mut state = lock(interp);
            let c = state.oo.class_mut(obj);
            if cmd == "constructor" {
                c.constructor = method;
            } else {
                c.destructor = method;
            }
            empty
        }
        "method" | "forward" => {
            let (name, flags, imp) = if cmd == "method" {
                let (name, option, params, body) = match args {
                    [name, params, body] => (name, None, params, body),
                    [name, option, params, body] => (name, Some(option), params, body),
                    _ => return Err(wrong_args(&usage_of(written, "name ?option? args body"))),
                };
                let flags = match option {
                    Some(o) => {
                        match index_of("export flag", o, &["-export", "-private", "-unexport"])? {
                            "-export" => PUBLIC_METHOD,
                            "-private" => TRUE_PRIVATE_METHOD,
                            _ => 0,
                        }
                    }
                    None if private => TRUE_PRIVATE_METHOD,
                    None => public_by_default(name),
                };
                (name, flags, proc_method(params, body)?)
            } else {
                let [name, prefix @ ..] = args else {
                    return Err(wrong_args(&usage_of(written, "name cmdName ?arg ...?")));
                };
                if prefix.is_empty() {
                    return Err(wrong_args(&usage_of(written, "name cmdName ?arg ...?")));
                }
                let flags = if private {
                    TRUE_PRIVATE_METHOD
                } else {
                    public_by_default(name)
                };
                (name, flags, Arc::new(Imp::Forward(prefix.to_vec())))
            };
            let mut state = lock(interp);
            table_of(&mut state.oo, obj, class).insert(
                name.clone(),
                Method {
                    flags,
                    imp: Some(imp),
                },
            );
            empty
        }
        "deletemethod" => {
            if args.is_empty() {
                return Err(wrong_args(&usage_of(written, "name ?name ...?")));
            }
            let mut state = lock(interp);
            let table = table_of(&mut state.oo, obj, class);
            for name in args {
                if table.remove(name).is_none() {
                    return Err(coded_err(
                        format!("method {name} does not exist"),
                        &["TCL", "LOOKUP", "METHOD", name],
                    ));
                }
            }
            empty
        }
        "renamemethod" => {
            let [from, to] = args else {
                return Err(wrong_args(&usage_of(written, "fromName toName")));
            };
            let mut state = lock(interp);
            let table = table_of(&mut state.oo, obj, class);
            if !table.contains_key(from) {
                return Err(coded_err(
                    format!("method {from} does not exist"),
                    &["TCL", "LOOKUP", "METHOD", from],
                ));
            }
            if from == to {
                return Err(oo_err("cannot rename method to itself", "RENAME_TO_SELF"));
            }
            if table.contains_key(to) {
                return Err(oo_err(
                    format!("method called {to} already exists"),
                    "RENAME_OVER",
                ));
            }
            let m = table.remove(from).expect("checked above");
            table.insert(to.clone(), m);
            empty
        }
        "export" | "unexport" => {
            if args.is_empty() {
                return Err(wrong_args(&usage_of(written, "name ?name ...?")));
            }
            let mut state = lock(interp);
            let table = table_of(&mut state.oo, obj, class);
            for name in args {
                let m = table.entry(name.clone()).or_insert(Method {
                    flags: 0,
                    imp: None,
                });
                if cmd == "export" {
                    m.flags |= PUBLIC_METHOD;
                    m.flags &= !TRUE_PRIVATE_METHOD;
                } else {
                    m.flags &= !(PUBLIC_METHOD | TRUE_PRIVATE_METHOD);
                }
            }
            empty
        }
        // The slots are objects (`TclOODefineSlots`): `::oo::define::superclass`
        // and the rest, called as the definition wrote them.
        "superclass" | "mixin" | "filter" | "variable" => {
            let space = if class {
                "::oo::define"
            } else {
                "::oo::objdefine"
            };
            let slot = lock(interp)
                .oo
                .lookup(&format!("{space}::{cmd}"), "::")
                .ok_or_else(|| not_an_object(cmd))?;
            let words: Vec<Value> = args.iter().map(|a| val(a.as_str())).collect();
            invoke_object(interp, vm, slot, true, written, space, &words)
        }
        "self" => {
            if !class {
                return match args {
                    [] => Ok(val(lock(interp).oo.name(obj))),
                    _ => Err(wrong_args(written)),
                };
            }
            match args {
                [] => Ok(val(lock(interp).oo.name(obj))),
                [script] => {
                    run_definition(interp, vm, obj, false, private, ns, "class object", script)
                }
                [sub, rest @ ..] => {
                    let Some(sub_cmd) = define_prefix(sub, OBJDEFINE_CMDS) else {
                        return Err(TclError::plain(format!("invalid command name \"{sub}\"")));
                    };
                    let frame = Frame::Define {
                        obj,
                        class: false,
                        private,
                        outer: outer.clone(),
                        space: definition_space(interp, obj, false)?,
                    };
                    let usage = format!("{written} {sub}");
                    with_frame(interp, frame, || {
                        define_sub(interp, vm, sub_cmd, &usage, rest)
                    })
                }
            }
        }
        "private" => match args {
            [] => Ok(val(if private { "1" } else { "0" })),
            [script] => {
                let kind = if class { "class" } else { "object" };
                run_definition(interp, vm, obj, class, true, ns, kind, script)
            }
            [sub, rest @ ..] => {
                let table = if class { DEFINE_CMDS } else { OBJDEFINE_CMDS };
                let Some(sub_cmd) = define_prefix(sub, table) else {
                    return Err(TclError::plain(format!("invalid command name \"{sub}\"")));
                };
                let frame = Frame::Define {
                    obj,
                    class,
                    private: true,
                    outer: outer.clone(),
                    space: space.clone(),
                };
                let usage = format!("{written} {sub}");
                with_frame(interp, frame, || {
                    define_sub(interp, vm, sub_cmd, &usage, rest)
                })
            }
        },
        "class" => {
            let [name] = args else {
                return Err(wrong_args(&usage_of(written, "className")));
            };
            let mut state = lock(interp);
            let oo = &mut state.oo;
            if obj == oo.object_cls {
                return Err(oo_err(
                    "may not modify the class of the root object class",
                    "MONKEY_BUSINESS",
                ));
            }
            if obj == oo.class_cls {
                return Err(oo_err(
                    "may not modify the class of the class of classes",
                    "MONKEY_BUSINESS",
                ));
            }
            let cls = oo.lookup(name, ns).ok_or_else(|| not_an_object(name))?;
            if !oo.is_class(cls) {
                return Err(coded_err(
                    "the class of an object must be a class",
                    &["TCL", "LOOKUP", "CLASS", name],
                ));
            }
            if cls == obj {
                return Err(oo_err(
                    "may not change classes into an instance of themselves",
                    "MONKEY_BUSINESS",
                ));
            }
            let old = oo.obj(obj).class;
            if old != cls {
                oo.class_mut(old).instances.retain(|&i| i != obj);
                oo.obj_mut(obj).class = cls;
                oo.class_mut(cls).instances.push(obj);
                // An object made an instance of a metaclass becomes a class.
                if !oo.is_class(obj) && oo.reachable(oo.class_cls, cls) {
                    let root = oo.object_cls;
                    oo.obj_mut(obj).class_data = Some(ClassData {
                        superclasses: vec![root],
                        ..ClassData::default()
                    });
                    oo.class_mut(root).subclasses.push(obj);
                }
            }
            empty
        }
        "initialise" | "initialize" => {
            let [script] = args else {
                return Err(wrong_args(&usage_of(written, "script")));
            };
            let class_ns = lock(interp).oo.ns_of(obj);
            let src = format!(
                "namespace eval {} {}",
                crate::list::quote(&class_ns, false),
                crate::list::quote(script, false)
            );
            crate::runtime::with_written_back(interp, vm, |interp| {
                crate::runtime::run_source(interp, &src)
            })
        }
        // `TclOODefineClassMethodObjCmd`: the method goes on the class's
        // delegate, and the class gets a forward `myclass name` to it.
        "property" | "properties" => define_property(interp, !class, args),
        "classmethod" => {
            let (name, body) = match args {
                [name] => (name, None),
                [name, params, body] => (name, Some((params, body))),
                _ => return Err(wrong_args(&usage_of(written, "name ?args body?"))),
            };
            let public = public_by_default(name) & PUBLIC_METHOD;
            if let Some((params, body)) = body {
                let delegate_name = format!("{}:: oo ::delegate", lock(interp).oo.ns_of(obj));
                let delegate = class_of(&lock(interp).oo, &delegate_name, "::")?;
                let flags = if private { 0 } else { public };
                let imp = proc_method(params, body)?;
                lock(interp).oo.class_mut(delegate).methods.insert(
                    name.clone(),
                    Method {
                        flags,
                        imp: Some(imp),
                    },
                );
            }
            let flags = if private { TRUE_PRIVATE_METHOD } else { public };
            let prefix = vec!["myclass".to_string(), name.clone()];
            lock(interp).oo.class_mut(obj).methods.insert(
                name.clone(),
                Method {
                    flags,
                    imp: Some(Arc::new(Imp::Forward(prefix))),
                },
            );
            empty
        }
        // `TclOODefineDefnNsObjCmd`.
        "definitionnamespace" => {
            {
                let oo = &lock(interp).oo;
                if obj == oo.object_cls || obj == oo.class_cls {
                    return Err(oo_err(
                        "may not modify the definition namespace of the root classes",
                        "MONKEY_BUSINESS",
                    ));
                }
            }
            let (instance, name) = match args {
                [name] => (false, name),
                [kind, name] => (
                    index_of("kind", kind, &["-class", "-instance"])? == "-instance",
                    name,
                ),
                _ => return Err(wrong_args(&usage_of(written, "?kind? namespace"))),
            };
            let target = if name.is_empty() {
                None
            } else {
                let fqn = resolve(ns, name);
                if !lock(interp).ns.exists(&fqn) {
                    return Err(crate::cmd_namespace::namespace_not_found(name, ns));
                }
                Some(fqn)
            };
            let mut state = lock(interp);
            let c = state.oo.class_mut(obj);
            if instance {
                c.obj_def_ns = target;
            } else {
                c.cls_def_ns = target;
            }
            empty
        }
        other => Err(TclError::plain(format!("invalid command name \"{other}\""))),
    }
}

/// The definition being run, for a slot's accessors (`TclOOGetDefineCmdContext`):
/// the object, whether it is `oo::define`'s class definition, whether the
/// definition is `private`, and the namespace class names resolve from.
fn define_context(interp: &Shared) -> Result<(Id, bool, bool, String), TclError> {
    let state = lock(interp);
    let Some(Frame::Define {
        obj,
        class,
        private,
        outer,
        ..
    }) = state.oo.stack.last()
    else {
        return Err(oo_err(
            "this command may only be called from within the context of an ::oo::define or \
             ::oo::objdefine command",
            "MONKEY_BUSINESS",
        ));
    };
    if !state.oo.alive(*obj) {
        return Err(oo_err(
            "this command cannot be called when the object has been deleted",
            "MONKEY_BUSINESS",
        ));
    }
    Ok((*obj, *class, *private, outer.clone()))
}

/// A slot's `Get` (`ClassFilter_Get`, `ObjMixin_Get` and the rest): the
/// contents of the slot of the object being defined. `class` is whether the
/// slot is one of `oo::define`'s, which needs a class.
fn slot_get(
    interp: &Shared,
    which: Slot,
    class: bool,
    usage: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let (obj, _, private, _) = define_context(interp)?;
    let state = lock(interp);
    let oo = &state.oo;
    if class && !oo.is_class(obj) {
        return Err(oo_err("attempt to misuse API", "MONKEY_BUSINESS"));
    }
    if !args.is_empty() {
        return Err(wrong_args(usage));
    }
    let names = |ids: &[Id]| ids.iter().map(|&s| oo.name(s)).collect::<Vec<_>>();
    let list = match (which, class) {
        (Slot::Superclass, _) => names(&oo.class(obj).superclasses),
        (Slot::Mixin, true) => names(&oo.class(obj).mixins),
        (Slot::Mixin, false) => names(&oo.obj(obj).mixins),
        (Slot::Filter, true) => oo.class(obj).filters.clone(),
        (Slot::Filter, false) => oo.obj(obj).filters.clone(),
        (Slot::Variable, true) if private => oo.class(obj).private_vars.clone(),
        (Slot::Variable, false) if private => oo.obj(obj).private_vars.clone(),
        (Slot::Variable, true) => oo.class(obj).variables.clone(),
        (Slot::Variable, false) => oo.obj(obj).variables.clone(),
        (Slot::ReadableProps, true) => oo.class(obj).properties.readable.clone(),
        (Slot::ReadableProps, false) => oo.obj(obj).properties.readable.clone(),
        (Slot::WritableProps, true) => oo.class(obj).properties.writable.clone(),
        (Slot::WritableProps, false) => oo.obj(obj).properties.writable.clone(),
    };
    Ok(val(crate::list::join(&list)))
}

/// A slot's `Set` (`ClassSuper_Set`, `ObjVars_Set` and the rest): replace the
/// contents of the slot of the object being defined with one list.
fn slot_set(
    interp: &Shared,
    which: Slot,
    class: bool,
    usage: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let (obj, _, private, ns) = define_context(interp)?;
    let mut state = lock(interp);
    let oo = &mut state.oo;
    if class && !oo.is_class(obj) {
        return Err(oo_err("attempt to misuse API", "MONKEY_BUSINESS"));
    }
    let [list] = args else {
        let what = match (which, class) {
            (Slot::Superclass, _) => "superclassList",
            (Slot::Mixin, _) => "mixinList",
            (Slot::Variable, false) => "variableList",
            _ => "filterList",
        };
        return Err(wrong_args(&usage_of(usage, what)));
    };
    let items = crate::list::split(&to_tcl_string(list)).map_err(TclError::plain)?;
    match which {
        Slot::Superclass => set_superclasses(oo, obj, &items, &ns)?,
        Slot::ReadableProps | Slot::WritableProps => {
            // `SetPropertyList`: a set, first occurrence kept.
            let mut unique: Vec<String> = Vec::with_capacity(items.len());
            for p in items {
                if !unique.contains(&p) {
                    unique.push(p);
                }
            }
            let props = if class {
                &mut oo.class_mut(obj).properties
            } else {
                &mut oo.obj_mut(obj).properties
            };
            if which == Slot::ReadableProps {
                props.readable = unique;
            } else {
                props.writable = unique;
            }
        }
        Slot::Mixin => set_mixins(oo, obj, class, &items, &ns)?,
        Slot::Filter => {
            if class {
                oo.class_mut(obj).filters = items;
            } else {
                oo.obj_mut(obj).filters = items;
            }
        }
        Slot::Variable => {
            for v in &items {
                let why = if v.contains("::") {
                    "contain namespace separators"
                } else if crate::assoc::string_match(v, "*(*)") {
                    "refer to an array element"
                } else {
                    continue;
                };
                return Err(oo_err(
                    format!("invalid declared variable name \"{v}\": must not {why}"),
                    "BAD_DECLVAR",
                ));
            }
            let mut unique: Vec<String> = Vec::with_capacity(items.len());
            for v in items {
                if !unique.contains(&v) {
                    unique.push(v);
                }
            }
            match (class, private) {
                (true, true) => {
                    let epoch = oo.obj(obj).creation_id;
                    let c = oo.class_mut(obj);
                    c.private_vars = unique;
                    c.private_epoch = epoch;
                }
                (false, true) => {
                    let o = oo.obj_mut(obj);
                    o.private_vars = unique;
                    o.private_epoch = o.creation_id;
                }
                (true, false) => oo.class_mut(obj).variables = unique,
                (false, false) => oo.obj_mut(obj).variables = unique,
            }
        }
    }
    Ok(val(""))
}

/// `Slot_ResolveClass`: a slot element that names a class becomes the
/// class's own name; anything else is kept as written.
fn slot_resolve_class(interp: &Shared, usage: &str, args: &[Value]) -> Result<Value, TclError> {
    let (_, _, _, ns) = define_context(interp)?;
    let [item] = args else {
        return Err(wrong_args(&usage_of(usage, "slotElement")));
    };
    let item = to_tcl_string(item);
    let oo = &lock(interp).oo;
    Ok(val(match oo.lookup(&item, &ns) {
        Some(id) if oo.is_class(id) => oo.name(id),
        _ => item,
    }))
}

/// Call one of slot `slot`'s own methods as `my` does (`CallSlotGet`,
/// `CallSlotSet`, `CallSlotResolve`).
fn slot_call(
    interp: &Shared,
    vm: &mut VM,
    slot: Id,
    method: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    let mut words = vec![val(method)];
    words.extend_from_slice(args);
    let ns = lock(interp).oo.ns_of(slot);
    invoke_object(interp, vm, slot, false, "my", &ns, &words)
}

/// The `oo::Slot` operations `-append`, `-appendifnew`, `-clear`,
/// `-prepend`, `-remove` and `-set` (`Slot_Append` and the rest), built on the
/// slot's own `Get`, `Set` and `Resolve`.
fn slot_op(
    interp: &Shared,
    vm: &mut VM,
    slot: Id,
    op: &str,
    usage: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    if op == "-clear" {
        if !args.is_empty() {
            return Err(wrong_args(usage));
        }
        return slot_call(interp, vm, slot, "Set", &[val("")]);
    }
    if args.is_empty() && op != "-set" {
        return Ok(val(""));
    }
    let mut resolved = Vec::with_capacity(args.len());
    for item in args {
        resolved.push(to_tcl_string(&slot_call(
            interp,
            vm,
            slot,
            "Resolve",
            std::slice::from_ref(item),
        )?));
    }
    let list: Vec<String> = match op {
        "-set" => resolved,
        _ => {
            let old = to_tcl_string(&slot_call(interp, vm, slot, "Get", &[])?);
            let old = crate::list::split(&old).map_err(TclError::plain)?;
            match op {
                "-append" => old.into_iter().chain(resolved).collect(),
                "-appendifnew" => {
                    let mut out = old;
                    for item in resolved {
                        if !out.contains(&item) {
                            out.push(item);
                        }
                    }
                    out
                }
                "-prepend" => resolved.into_iter().chain(old).collect(),
                _ => old.into_iter().filter(|o| !resolved.contains(o)).collect(),
            }
        }
    };
    slot_call(interp, vm, slot, "Set", &[val(crate::list::join(&list))])
}

// ── properties (`generic/tclOOProp.c`) ──────────────────────────────────

/// `BuildPropertyList` and its install: add `name` to `list` when `adding`
/// and absent, take it out when not `adding` and present.
fn register_property(list: &mut Vec<String>, name: &str, adding: bool) {
    let present = list.iter().any(|p| p == name);
    if adding && !present {
        list.push(name.to_string());
    } else if !adding && present {
        list.retain(|p| p != name);
    }
}

/// `TclOODefinePropertyCmd`: `property name ?-option value ...? ?name ...?`
/// in a configurable class's definition, or an instance's when `instance`.
fn define_property(interp: &Shared, instance: bool, args: &[String]) -> Result<Value, TclError> {
    let (obj, _, _, _) = define_context(interp)?;
    if !instance && !lock(interp).oo.is_class(obj) {
        return Err(oo_err("attempt to misuse API", "MONKEY_BUSINESS"));
    }
    let mut i = 0;
    while i < args.len() {
        let name = &args[i];
        let (mut getter, mut setter, mut kind) = (None, None, "readwrite");
        while i + 1 < args.len() && args[i + 1].starts_with('-') {
            let option = index_of("option", &args[i + 1], &["-get", "-kind", "-set"])?;
            let Some(value) = args.get(i + 2) else {
                let what = if option == "-kind" {
                    "kind value"
                } else {
                    "body"
                };
                return Err(coded_err(
                    format!("missing {what} to go with {option} option"),
                    &["TCL", "WRONGARGS"],
                ));
            };
            match option {
                "-get" => getter = Some(value.clone()),
                "-set" => setter = Some(value.clone()),
                _ => kind = index_of("kind", value, &["readable", "readwrite", "writable"])?,
            }
            i += 2;
        }
        // `InstallStdPropertyImpls`: the name is checked, then the standard
        // getter and setter go in where no body was given.
        let why = if name.starts_with('-') {
            Some("must not begin with -")
        } else if crate::list::quote(name, false) != *name {
            Some("must be a simple word")
        } else if name.contains("::") {
            Some("must not contain namespace separators")
        } else if name.contains('(') || name.contains(')') {
            Some("must not contain parentheses")
        } else {
            None
        };
        if let Some(why) = why {
            return Err(oo_err(
                format!("bad property name \"{name}\": {why}"),
                "PROPERTY_FORMAT",
            ));
        }
        let readable = kind != "writable";
        let writable = kind != "readable";
        let mut methods: Vec<(String, Arc<Imp>)> = Vec::new();
        let standard = |setter: bool| {
            Arc::new(Imp::Property {
                var: name.clone(),
                setter,
            })
        };
        match (readable, &getter) {
            (_, Some(body)) => methods.push((format!("<ReadProp-{name}>"), proc_method("", body)?)),
            (true, None) => methods.push((format!("<ReadProp-{name}>"), standard(false))),
            (false, None) => {}
        }
        match (writable, &setter) {
            (_, Some(body)) => {
                methods.push((format!("<WriteProp-{name}>"), proc_method("value", body)?))
            }
            (true, None) => methods.push((format!("<WriteProp-{name}>"), standard(true))),
            (false, None) => {}
        }
        let hyphenated = format!("-{name}");
        let mut state = lock(interp);
        let oo = &mut state.oo;
        let (table, props) = if instance {
            let o = oo.obj_mut(obj);
            (&mut o.methods, &mut o.properties)
        } else {
            let c = oo.class_mut(obj);
            (&mut c.methods, &mut c.properties)
        };
        for (method, imp) in methods {
            table.insert(
                method,
                Method {
                    flags: 0,
                    imp: Some(imp),
                },
            );
        }
        register_property(&mut props.readable, &hyphenated, readable);
        register_property(&mut props.writable, &hyphenated, writable);
        i += 1;
    }
    Ok(val(""))
}

impl Oo {
    /// `FindClassProps`: the properties of `cls`, its class mixins and its
    /// superclasses, stopping at the root object class.
    fn class_props(&self, cls: Id, writable: bool, into: &mut Vec<String>) {
        if !self.alive(cls) || !self.is_class(cls) {
            return;
        }
        let c = self.class(cls);
        let own = if writable {
            &c.properties.writable
        } else {
            &c.properties.readable
        };
        for p in own {
            if !into.contains(p) {
                into.push(p.clone());
            }
        }
        if cls == self.object_cls {
            return;
        }
        for &m in &c.mixins {
            self.class_props(m, writable, into);
        }
        for &s in &c.superclasses {
            self.class_props(s, writable, into);
        }
    }

    /// `GetAllObjectProperties`: the sorted properties of `obj`, its mixins and
    /// its class.
    fn object_props(&self, obj: Id, writable: bool) -> Vec<String> {
        let o = self.obj(obj);
        let mut all: Vec<String> = Vec::new();
        let own = if writable {
            &o.properties.writable
        } else {
            &o.properties.readable
        };
        for p in own {
            if !all.contains(p) {
                all.push(p.clone());
            }
        }
        for &m in &o.mixins {
            self.class_props(m, writable, &mut all);
        }
        self.class_props(o.class, writable, &mut all);
        all.sort();
        all
    }
}

/// `info class properties` and `info object properties`
/// (`TclOOInfoClassPropCmd`, `TclOOInfoObjectPropCmd`).
fn info_properties(
    oo: &Oo,
    target: Id,
    class: bool,
    options: &[String],
) -> Result<String, TclError> {
    let (mut all, mut writable) = (false, false);
    for o in options {
        match index_of("option", o, &["-all", "-readable", "-writable"])? {
            "-all" => all = true,
            "-readable" => writable = false,
            _ => writable = true,
        }
    }
    let mut list = match (all, class) {
        (true, true) => {
            let mut v = Vec::new();
            oo.class_props(target, writable, &mut v);
            v
        }
        (true, false) => oo.object_props(target, writable),
        (false, true) => {
            let p = &oo.class(target).properties;
            if writable {
                p.writable.clone()
            } else {
                p.readable.clone()
            }
        }
        (false, false) => {
            let p = &oo.obj(target).properties;
            if writable {
                p.writable.clone()
            } else {
                p.readable.clone()
            }
        }
    };
    list.sort();
    Ok(crate::list::join(&list))
}

/// `Configurable_Getter` and `Configurable_Setter`: the object variable named
/// after the property, read or written.
fn property_access(
    interp: &Shared,
    vm: &mut VM,
    obj: Id,
    var: &str,
    setter: bool,
    args: &[Value],
    usage: &str,
) -> Result<Value, TclError> {
    match (setter, args) {
        (false, []) | (true, [_]) => {}
        (false, _) => return Err(wrong_args(usage)),
        (true, _) => return Err(wrong_args(&usage_of(usage, "value"))),
    }
    crate::runtime::with_written_back(interp, vm, |interp| {
        let mut state = lock(interp);
        let name = state.oo.private_var_name(obj, var);
        let key = format!("{}::{name}", store_key(&state.oo.ns_of(obj)));
        if let [value] = args {
            state.globals.insert(key, value.clone());
            return Ok(value.clone());
        }
        match state.globals.get(&key) {
            Some(Value::Undef) | None => Err(coded_err(
                format!("can't read \"{var}\": no such variable"),
                &["TCL", "READ", "VARNAME"],
            )),
            Some(v) => Ok(v.clone()),
        }
    })
}

/// `GetPropertyName`: the property `given` names (a unique prefix will do)
/// among the object's readable or writable ones, or the refusal — worded
/// `property "-x" is read only` when the other set has it.
fn property_name(oo: &Oo, obj: Id, writable: bool, given: &str) -> Result<String, TclError> {
    let names = oo.object_props(obj, writable);
    let table: Vec<&str> = names.iter().map(String::as_str).collect();
    match index_of("property", given, &table) {
        Ok(found) => Ok(found.to_string()),
        Err(mut e) => {
            let others = oo.object_props(obj, !writable);
            let others: Vec<&str> = others.iter().map(String::as_str).collect();
            if let Ok(other) = index_of("property", given, &others) {
                let only = if writable { "read" } else { "write" };
                e.msg = format!("property \"{other}\" is {only} only");
            }
            Err(e)
        }
    }
}

/// `TclOO_Configurable_Configure`.
fn configure(
    interp: &Shared,
    vm: &mut VM,
    obj: Id,
    usage: &str,
    args: &[Value],
) -> Result<Value, TclError> {
    if args.len() % 2 == 1 && args.len() != 1 {
        return Err(wrong_args(&usage_of(usage, "?-option value ...?")));
    }
    let read = |interp: &Shared, vm: &mut VM, name: &str| {
        slot_call(interp, vm, obj, &format!("<ReadProp{name}>"), &[])
    };
    match args {
        [] => {
            let names = lock(interp).oo.object_props(obj, false);
            let mut out = Vec::with_capacity(names.len() * 2);
            for name in names {
                let value = read(interp, vm, &name)?;
                out.push(name);
                out.push(to_tcl_string(&value));
            }
            Ok(val(crate::list::join(&out)))
        }
        [one] => {
            let name = property_name(&lock(interp).oo, obj, false, &to_tcl_string(one))?;
            read(interp, vm, &name)
        }
        _ => {
            for pair in args.chunks(2) {
                let name = property_name(&lock(interp).oo, obj, true, &to_tcl_string(&pair[0]))?;
                slot_call(interp, vm, obj, &format!("<WriteProp{name}>"), &pair[1..])?;
            }
            Ok(val(""))
        }
    }
}

/// The class a definition names, refused with `msg` when it names an object
/// that is not a class (`GetClassInOuterContext`).
fn class_named(oo: &Oo, name: &str, ns: &str, msg: &str) -> Result<Id, TclError> {
    let id = oo.lookup(name, ns).ok_or_else(|| not_an_object(name))?;
    if !oo.is_class(id) {
        return Err(coded_err(msg, &["TCL", "LOOKUP", "CLASS", name]));
    }
    Ok(id)
}

/// `ClassSuper_Set`.
fn set_superclasses(oo: &mut Oo, cls: Id, names: &[String], ns: &str) -> Result<(), TclError> {
    if cls == oo.object_cls {
        return Err(oo_err(
            "may not modify the superclass of the root object",
            "MONKEY_BUSINESS",
        ));
    }
    let mut supers: Vec<Id> = Vec::new();
    if names.is_empty() {
        supers.push(if oo.reachable(oo.class_cls, cls) {
            oo.class_cls
        } else {
            oo.object_cls
        });
    }
    for name in names {
        let s = class_named(oo, name, ns, "only a class can be a superclass")?;
        if supers.contains(&s) {
            return Err(oo_err(
                "class should only be a direct superclass once",
                "REPETITIOUS",
            ));
        }
        if oo.reachable(cls, s) {
            return Err(oo_err(
                "attempt to form circular dependency graph",
                "CIRCULARITY",
            ));
        }
        supers.push(s);
    }
    replace_superclasses(oo, cls, supers);
    Ok(())
}

/// `TclOOSetSuperclasses`: splice `cls` out of its old superclasses' subclass
/// lists and into the new ones'.
fn replace_superclasses(oo: &mut Oo, cls: Id, supers: Vec<Id>) {
    let old = std::mem::take(&mut oo.class_mut(cls).superclasses);
    for s in old {
        if oo.alive(s) {
            oo.class_mut(s).subclasses.retain(|&i| i != cls);
        }
    }
    for &s in &supers {
        oo.class_mut(s).subclasses.push(cls);
    }
    oo.class_mut(cls).superclasses = supers;
}

/// `ClassMixin_Set` and `ObjMixin_Set`.
fn set_mixins(
    oo: &mut Oo,
    obj: Id,
    class: bool,
    names: &[String],
    ns: &str,
) -> Result<(), TclError> {
    let mut mixins: Vec<Id> = Vec::new();
    for name in names {
        let m = class_named(oo, name, ns, "may only mix in classes")?;
        if mixins.contains(&m) {
            return Err(oo_err(
                "class should only be a direct mixin once",
                "REPETITIOUS",
            ));
        }
        if class && oo.reachable(obj, m) {
            return Err(oo_err("may not mix a class into itself", "SELF_MIXIN"));
        }
        mixins.push(m);
    }
    if class {
        let old = std::mem::take(&mut oo.class_mut(obj).mixins);
        for m in old {
            if oo.alive(m) {
                oo.class_mut(m).mixin_users.retain(|&i| i != obj);
            }
        }
        for &m in &mixins {
            oo.class_mut(m).mixin_users.push(obj);
        }
        oo.class_mut(obj).mixins = mixins;
    } else {
        let own = oo.obj(obj).class;
        let old = std::mem::take(&mut oo.obj_mut(obj).mixins);
        for m in old {
            if oo.alive(m) && m != own {
                oo.class_mut(m).instances.retain(|&i| i != obj);
            }
        }
        for &m in &mixins {
            if m != own {
                oo.class_mut(m).instances.push(obj);
            }
        }
        oo.obj_mut(obj).mixins = mixins;
    }
    Ok(())
}

// ── oo::copy ─────────────────────────────────────────────────────────────

/// `TclOOCopyObjectCmd` and `Tcl_CopyObjectInstance`: a new object with the
/// source's class, methods, mixins, filters and declarations (and, for a
/// class, its class half), then `<cloned>` called on it. No constructor runs.
fn copy_cmd(interp: &Shared, vm: &mut VM, ns: &str, args: &[Value]) -> Result<Value, TclError> {
    let words = strings(args);
    if words.is_empty() || words.len() > 3 {
        return Err(wrong_args(
            "oo::copy sourceName ?targetName? ?targetNamespace?",
        ));
    }
    let id = {
        let mut state = lock(interp);
        let src = state
            .oo
            .lookup(&words[0], ns)
            .ok_or_else(|| not_an_object(&words[0]))?;
        let name = words
            .get(1)
            .filter(|n| !n.is_empty())
            .map(|n| resolve(ns, n));
        if let Some(fqn) = &name {
            if command_exists(&state, fqn) {
                return Err(oo_err(
                    format!(
                        "can't create object \"{}\": command already exists with that name",
                        words[1]
                    ),
                    "OVERWRITE_OBJECT",
                ));
            }
        }
        let target_ns = words
            .get(2)
            .filter(|n| !n.is_empty())
            .map(|n| resolve(ns, n));
        if let Some(t) = &target_ns {
            if state.ns.exists(t) {
                return Err(TclError::plain(format!(
                    "{} refers to an existing namespace",
                    words[2]
                )));
            }
        }
        let crate::runtime::State { oo, ns: reg, .. } = &mut *state;
        let class = oo.obj(src).class;
        let id = oo.alloc(reg, class, name, target_ns);
        let (methods, mixins, filters, variables, privates) = {
            let s = oo.obj(src);
            (
                s.methods.clone(),
                s.mixins.clone(),
                s.filters.clone(),
                s.variables.clone(),
                (s.private_vars.clone(), s.private_epoch),
            )
        };
        let mixin_names: Vec<String> = mixins.iter().map(|&m| oo.name(m)).collect();
        {
            let o = oo.obj_mut(id);
            o.methods = methods;
            o.filters = filters;
            o.variables = variables;
            (o.private_vars, o.private_epoch) = privates;
        }
        set_mixins(oo, id, false, &mixin_names, "::")?;
        if oo.is_class(src) && oo.is_class(id) {
            let (supers, methods, ctor, dtor, mixins, filters, variables, privates) = {
                let c = oo.class(src);
                (
                    c.superclasses.clone(),
                    c.methods.clone(),
                    c.constructor.clone(),
                    c.destructor.clone(),
                    c.mixins.clone(),
                    c.filters.clone(),
                    c.variables.clone(),
                    (c.private_vars.clone(), c.private_epoch),
                )
            };
            replace_superclasses(oo, id, supers);
            let mixin_names: Vec<String> = mixins.iter().map(|&m| oo.name(m)).collect();
            {
                let c = oo.class_mut(id);
                c.methods = methods;
                c.constructor = ctor;
                c.destructor = dtor;
                c.filters = filters;
                c.variables = variables;
                (c.private_vars, c.private_epoch) = privates;
            }
            set_mixins(oo, id, true, &mixin_names, "::")?;
        }
        (id, oo.name(src))
    };
    let (id, source) = id;
    let chain = lock(interp)
        .oo
        .chain_for(id, Some("<cloned>"), 0, None, None);
    if let Some(chain) = chain {
        let usage = format!("{} <cloned>", lock(interp).oo.name(id));
        if let Err(e) = invoke_context(
            interp,
            vm,
            id,
            Arc::new(chain),
            0,
            &[val(source)],
            &usage,
            ns,
        ) {
            let _ = destroy_object(interp, vm, id);
            return Err(e);
        }
    }
    Ok(val(lock(interp).oo.name(id)))
}

// ── info object and info class ───────────────────────────────────────────

const INFO_OBJECT_CMDS: &[&str] = &[
    "call",
    "class",
    "creationid",
    "definition",
    "filters",
    "forward",
    "isa",
    "methods",
    "methodtype",
    "mixins",
    "namespace",
    "properties",
    "variables",
    "vars",
];

const INFO_CLASS_CMDS: &[&str] = &[
    "call",
    "constructor",
    "definition",
    "definitionnamespace",
    "destructor",
    "filters",
    "forward",
    "instances",
    "methods",
    "methodtype",
    "mixins",
    "properties",
    "subclasses",
    "superclasses",
    "variables",
];

/// An ensemble's subcommand by unique prefix, refused as the ensemble
/// machinery refuses one.
fn ensemble_sub<'a>(given: &str, table: &[&'a str]) -> Result<&'a str, TclError> {
    if let Some(exact) = table.iter().find(|t| **t == given) {
        return Ok(exact);
    }
    let hits: Vec<&&str> = table
        .iter()
        .filter(|t| !given.is_empty() && t.starts_with(given))
        .collect();
    if let [one] = hits.as_slice() {
        return Ok(one);
    }
    let list = match table {
        [init @ .., last] => format!("{}, or {last}", init.join(", ")),
        [] => String::new(),
    };
    Err(coded_err(
        format!("unknown or ambiguous subcommand \"{given}\": must be {list}"),
        &["TCL", "LOOKUP", "SUBCOMMAND", given],
    ))
}

/// `{name ?default?} …` and the body, as `info … definition` answers.
fn definition_of(p: &ProcImp) -> String {
    let params: Vec<String> = p
        .sig
        .params
        .iter()
        .map(|x| match &x.default {
            Some(d) => crate::list::join(&[x.name.as_str(), d.as_str()]),
            None => crate::list::join(&[x.name.as_str()]),
        })
        .collect();
    crate::list::join(&[crate::list::join(&params), p.body.clone()])
}

fn unknown_method_named(name: &str) -> TclError {
    coded_err(
        format!("unknown method \"{name}\""),
        &["TCL", "LOOKUP", "METHOD", name],
    )
}

/// `info object methods` / `info class methods` options.
fn method_list_flags(words: &[String]) -> Result<(u32, bool, Option<u32>), TclError> {
    let mut flag = PUBLIC_METHOD;
    let mut recurse = false;
    let mut scope = None;
    let mut i = 0;
    while i < words.len() {
        match index_of(
            "option",
            &words[i],
            &["-all", "-localprivate", "-private", "-scope"],
        )? {
            "-all" => recurse = true,
            "-localprivate" => flag = 0x02,
            "-private" => flag = 0,
            _ => {
                i += 1;
                let Some(s) = words.get(i) else {
                    return Err(coded_err(
                        "missing option for -scope",
                        &["TCL", "ARGUMENT", "MISSING"],
                    ));
                };
                scope = Some(
                    match index_of("scope", s, &["private", "public", "unexported"])? {
                        "private" => TRUE_PRIVATE_METHOD,
                        "public" => PUBLIC_METHOD,
                        _ => 0,
                    },
                );
            }
        }
        i += 1;
    }
    if let Some(s) = scope {
        return Ok((s, false, Some(s)));
    }
    Ok((flag, recurse, None))
}

/// The methods of one table `info … methods` lists without `-all`.
fn local_methods(table: &BTreeMap<String, Method>, flag: u32, scope: Option<u32>) -> Vec<String> {
    table
        .iter()
        .filter(|(_, m)| {
            m.imp.is_some()
                && match scope {
                    None => m.flags & (flag | TRUE_PRIVATE_METHOD) == flag,
                    Some(_) => m.flags & (PUBLIC_METHOD | 0x02 | TRUE_PRIVATE_METHOD) == flag,
                }
        })
        .map(|(n, _)| n.clone())
        .collect()
}

/// `info object subcommand objName …`.
fn info_object(interp: &Shared, ns: &str, args: &[Value]) -> Result<Value, TclError> {
    let words = strings(args);
    let Some(sub) = words.first() else {
        return Err(wrong_args("info object subcommand ?arg ...?"));
    };
    let sub = ensemble_sub(sub, INFO_OBJECT_CMDS)?;
    let rest = &words[1..];
    let usage = |u: &str| wrong_args(&format!("info object {sub} {u}"));
    let state = lock(interp);
    let oo = &state.oo;
    let object = |name: &str| oo.lookup(name, ns).ok_or_else(|| not_an_object(name));
    let names =
        |ids: &[Id]| crate::list::join(&ids.iter().map(|&i| oo.name(i)).collect::<Vec<_>>());
    let answer = match sub {
        "isa" => {
            let cats = ["class", "metaclass", "mixin", "object", "typeof"];
            if rest.len() < 2 {
                return Err(usage("category objName ?arg ...?"));
            }
            let cat = index_of("category", &rest[0], &cats)?;
            let arity = if matches!(cat, "mixin" | "typeof") {
                3
            } else {
                2
            };
            if rest.len() != arity {
                let u = if arity == 3 {
                    "objName className"
                } else {
                    "objName"
                };
                return Err(wrong_args(&format!("info object isa {cat} {u}")));
            }
            let Some(o) = oo.lookup(&rest[1], ns) else {
                return Ok(val("0"));
            };
            let yes = match cat {
                "object" => true,
                "class" => oo.is_class(o),
                "metaclass" => oo.is_class(o) && oo.reachable(oo.class_cls, o),
                "mixin" => match oo.lookup(&rest[2], ns) {
                    Some(c) if oo.is_class(c) => {
                        oo.obj(o).mixins.iter().any(|&m| oo.reachable(c, m))
                    }
                    _ => false,
                },
                _ => match oo.lookup(&rest[2], ns) {
                    Some(c) if oo.is_class(c) => oo.reachable(c, oo.obj(o).class),
                    _ => false,
                },
            };
            if yes { "1" } else { "0" }.to_string()
        }
        "class" => match rest {
            [name] => oo.name(oo.obj(object(name)?).class),
            [name, cls] => {
                let o = object(name)?;
                let c = class_of(oo, cls, ns)?;
                let o = oo.obj(o);
                let yes = o.mixins.iter().any(|&m| oo.reachable(c, m)) || oo.reachable(c, o.class);
                if yes { "1" } else { "0" }.to_string()
            }
            _ => return Err(usage("objName ?className?")),
        },
        "creationid" => match rest {
            [name] => oo.obj(object(name)?).creation_id.to_string(),
            _ => return Err(usage("objName")),
        },
        "namespace" => match rest {
            [name] => oo.obj(object(name)?).ns.clone(),
            _ => return Err(usage("objName")),
        },
        "filters" => match rest {
            [name] => crate::list::join(&oo.obj(object(name)?).filters),
            _ => return Err(usage("objName")),
        },
        "mixins" => match rest {
            [name] => names(&oo.obj(object(name)?).mixins),
            _ => return Err(usage("objName")),
        },
        "variables" => match rest {
            [name] => crate::list::join(&oo.obj(object(name)?).variables),
            [name, opt] => {
                index_of("option", opt, &["-private"])?;
                crate::list::join(&oo.obj(object(name)?).private_vars)
            }
            _ => return Err(usage("objName ?-private?")),
        },
        "vars" => {
            let (name, pattern) = match rest {
                [name] => (name, None),
                [name, p] => (name, Some(p.as_str())),
                _ => return Err(usage("objName ?pattern?")),
            };
            let prefix = format!("{}::", store_key(&oo.obj(object(name)?).ns));
            let mut vars: Vec<String> = state
                .globals
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix))
                .filter(|v| !v.contains("::"))
                .filter(|v| pattern.is_none_or(|p| crate::assoc::string_match(v, p)))
                .map(str::to_string)
                .collect();
            vars.sort();
            crate::list::join(&vars)
        }
        "methods" => {
            let [name, opts @ ..] = rest else {
                return Err(usage("objName ?-option value ...?"));
            };
            let o = object(name)?;
            let (flag, recurse, scope) = method_list_flags(opts)?;
            let list = if recurse {
                oo.sorted_methods(o, None, None, flag)
            } else {
                local_methods(&oo.obj(o).methods, flag, scope)
            };
            crate::list::join(&list)
        }
        "definition" | "forward" | "methodtype" => {
            let [name, method] = rest else {
                return Err(usage("objName methodName"));
            };
            let o = object(name)?;
            let Some(Method { imp: Some(imp), .. }) = oo.obj(o).methods.get(method) else {
                return Err(unknown_method_named(method));
            };
            describe_method(sub, imp, method)?
        }
        "call" => {
            let [name, method] = rest else {
                return Err(usage("objName methodName"));
            };
            let o = object(name)?;
            match oo.chain_for(o, Some(method), PUBLIC_METHOD, None, None) {
                Some(chain) => oo.render_chain(&chain),
                None => return Err(TclError::plain("cannot construct any call chain")),
            }
        }
        _ => {
            let [name, options @ ..] = rest else {
                return Err(usage("objName ?options...?"));
            };
            info_properties(oo, object(name)?, false, options)?
        }
    };
    Ok(val(answer))
}

/// `TclOOGetClassFromObj`.
fn class_of(oo: &Oo, name: &str, ns: &str) -> Result<Id, TclError> {
    let id = oo.lookup(name, ns).ok_or_else(|| not_an_object(name))?;
    if !oo.is_class(id) {
        return Err(coded_err(
            format!("\"{name}\" is not a class"),
            &["TCL", "LOOKUP", "CLASS", name],
        ));
    }
    Ok(id)
}

/// `definition`, `forward` and `methodtype` of one method.
fn describe_method(sub: &str, imp: &Imp, method: &str) -> Result<String, TclError> {
    match (sub, imp) {
        ("methodtype", imp) => Ok(method_type(imp, method)),
        ("definition", Imp::Proc(p)) => Ok(definition_of(p)),
        ("definition", _) => Err(coded_err(
            "definition not available for this kind of method",
            &["TCL", "LOOKUP", "METHOD", method],
        )),
        (_, Imp::Forward(prefix)) => Ok(crate::list::join(prefix)),
        _ => Err(coded_err(
            "prefix argument list not available for this kind of method",
            &["TCL", "LOOKUP", "METHOD", method],
        )),
    }
}

/// `info class subcommand className …`.
fn info_class(interp: &Shared, ns: &str, args: &[Value]) -> Result<Value, TclError> {
    let words = strings(args);
    let Some(sub) = words.first() else {
        return Err(wrong_args("info class subcommand ?arg ...?"));
    };
    let sub = ensemble_sub(sub, INFO_CLASS_CMDS)?;
    let rest = &words[1..];
    let usage = |u: &str| wrong_args(&format!("info class {sub} {u}"));
    let state = lock(interp);
    let oo = &state.oo;
    let class = |name: &str| class_of(oo, name, ns);
    let names =
        |ids: &[Id]| crate::list::join(&ids.iter().map(|&i| oo.name(i)).collect::<Vec<_>>());
    let matching = |ids: &[Id], pattern: Option<&String>| {
        let list: Vec<String> = ids
            .iter()
            .map(|&i| oo.name(i))
            .filter(|n| pattern.is_none_or(|p| crate::assoc::string_match(n, p)))
            .collect();
        crate::list::join(&list)
    };
    let answer = match sub {
        "superclasses" => match rest {
            [name] => names(&oo.class(class(name)?).superclasses),
            _ => return Err(usage("className")),
        },
        "subclasses" => match rest {
            [name] | [name, _] => matching(&oo.class(class(name)?).subclasses, rest.get(1)),
            _ => return Err(usage("className ?pattern?")),
        },
        "instances" => match rest {
            [name] | [name, _] => matching(&oo.class(class(name)?).instances, rest.get(1)),
            _ => return Err(usage("className ?pattern?")),
        },
        "mixins" => match rest {
            [name] => names(&oo.class(class(name)?).mixins),
            _ => return Err(usage("className")),
        },
        "filters" => match rest {
            [name] => crate::list::join(&oo.class(class(name)?).filters),
            _ => return Err(usage("className")),
        },
        "variables" => match rest {
            [name] => crate::list::join(&oo.class(class(name)?).variables),
            [name, opt] => {
                index_of("option", opt, &["-private"])?;
                crate::list::join(&oo.class(class(name)?).private_vars)
            }
            _ => return Err(usage("className ?-private?")),
        },
        "constructor" | "destructor" => {
            let [name] = rest else {
                return Err(usage("className"));
            };
            let c = oo.class(class(name)?);
            let m = if sub == "constructor" {
                &c.constructor
            } else {
                &c.destructor
            };
            match m.as_ref().and_then(|m| m.imp.as_deref()) {
                None => String::new(),
                Some(Imp::Proc(p)) if sub == "constructor" => definition_of(p),
                Some(Imp::Proc(p)) => p.body.clone(),
                Some(_) => {
                    return Err(coded_err(
                        "definition not available for this kind of method",
                        &["TCL", "LOOKUP", "METHOD", sub],
                    ))
                }
            }
        }
        "methods" => {
            let [name, opts @ ..] = rest else {
                return Err(usage("className ?-option value ...?"));
            };
            let c = class(name)?;
            let (flag, recurse, scope) = method_list_flags(opts)?;
            let list = if recurse {
                oo.sorted_class_methods(c, flag)
            } else {
                local_methods(&oo.class(c).methods, flag, scope)
            };
            crate::list::join(&list)
        }
        "definition" | "forward" | "methodtype" => {
            let [name, method] = rest else {
                return Err(usage("className methodName"));
            };
            let c = class(name)?;
            let Some(Method { imp: Some(imp), .. }) = oo.class(c).methods.get(method) else {
                return Err(unknown_method_named(method));
            };
            describe_method(sub, imp, method)?
        }
        "call" => {
            let [name, method] = rest else {
                return Err(usage("className methodName"));
            };
            let c = class(name)?;
            // `TclOOGetStereotypeCallChain`: the chain an instance with no
            // definitions of its own would have.
            let probe = oo.stereotype(c);
            match probe.chain_for(
                probe.objects.len() - 1,
                Some(method),
                PUBLIC_METHOD,
                None,
                None,
            ) {
                Some(chain) => oo.render_chain(&chain),
                None => return Err(TclError::plain("cannot construct any call chain")),
            }
        }
        "definitionnamespace" => {
            let (name, kind) = match rest {
                [name] => (name, None),
                [name, kind] => (name, Some(kind)),
                _ => return Err(usage("className ?kind?")),
            };
            let c = oo.class(class(name)?);
            let instance = match kind {
                Some(k) => index_of("kind", k, &["-class", "-instance"])? == "-instance",
                None => false,
            };
            let ns = if instance {
                &c.obj_def_ns
            } else {
                &c.cls_def_ns
            };
            ns.clone().unwrap_or_default()
        }
        _ => {
            let [name, options @ ..] = rest else {
                return Err(usage("className ?options...?"));
            };
            info_properties(oo, class(name)?, true, options)?
        }
    };
    Ok(val(answer))
}

impl Oo {
    /// A copy of the class graph with one more object: a bare instance of
    /// `cls`, for `info class call`.
    fn stereotype(&self, cls: Id) -> Oo {
        let copy = |o: &Object| Object {
            command: o.command.clone(),
            ns: o.ns.clone(),
            class: o.class,
            creation_id: o.creation_id,
            methods: o.methods.clone(),
            mixins: o.mixins.clone(),
            filters: o.filters.clone(),
            variables: o.variables.clone(),
            private_vars: o.private_vars.clone(),
            private_epoch: o.private_epoch,
            properties: o.properties.clone(),
            class_data: o.class_data.as_ref().map(|c| ClassData {
                superclasses: c.superclasses.clone(),
                mixins: c.mixins.clone(),
                filters: c.filters.clone(),
                methods: c.methods.clone(),
                constructor: c.constructor.clone(),
                destructor: c.destructor.clone(),
                ..ClassData::default()
            }),
            destructing: false,
            filter_handling: false,
        };
        let mut objects: Vec<Option<Object>> =
            self.objects.iter().map(|o| o.as_ref().map(copy)).collect();
        objects.push(Some(Object {
            command: String::new(),
            ns: String::new(),
            class: cls,
            creation_id: 0,
            methods: BTreeMap::new(),
            mixins: Vec::new(),
            filters: Vec::new(),
            variables: Vec::new(),
            private_vars: Vec::new(),
            private_epoch: 0,
            properties: Props::default(),
            class_data: None,
            destructing: false,
            filter_handling: false,
        }));
        Oo {
            objects,
            by_command: Default::default(),
            by_ns: Default::default(),
            ns_count: self.ns_count,
            stack: Vec::new(),
            object_cls: self.object_cls,
            class_cls: self.class_cls,
            graveyard: Default::default(),
            links: BTreeMap::new(),
            deleting: HashSet::new(),
        }
    }
}

/// A script this module ran as `namespace eval NS script` failed: drop the
/// wrapper from the error's trace and say what the script was instead —
/// `(in definition script for class "::C" line 3)` where tclsh's
/// `GenerateErrorInfo` says so.
fn relabel(mut e: TclError, wrapper: &str, label: impl Fn(usize) -> String) -> TclError {
    if let Some(info) = e.info.as_mut() {
        if info
            .entries
            .last()
            .is_some_and(|x| x.text.starts_with(wrapper))
        {
            info.entries.pop();
            if let Some(last) = info.entries.last_mut() {
                last.ctx = crate::errinfo::Ctx::Done {
                    line: Some(label(last.line)),
                    stack: "UP 1".to_string(),
                };
            }
        }
    }
    e
}

impl Oo {
    /// A command name a method body spelt in [`METHOD_NS`] — a `proc` the body
    /// defines, or a call to one — as the running object's namespace spells it.
    /// `None` for any other name, and for the compiled method bodies
    /// themselves, which live nowhere a script can see.
    pub(crate) fn method_command(&self, name: &str) -> Option<String> {
        let rest = store_key(name).strip_prefix(METHOD_NS_KEY)?;
        if rest.starts_with(METHOD_PROC_PREFIX) {
            return None;
        }
        let (obj, ..) = self.current_method()?;
        self.alive(obj)
            .then(|| format!("{}::{rest}", self.ns_of(obj)))
    }
}

/// `namespace delete` of an object's namespace: the object is destroyed with
/// it (`ObjectNamespaceDeleted`). Answers whether `fqn` was one.
pub(crate) fn namespace_deleted(interp: &Shared, vm: &mut VM, fqn: &str) -> bool {
    let (own, inside) = {
        let state = lock(interp);
        let prefix = format!("{}::", store_key(fqn));
        let mut inside: Vec<Id> = state
            .oo
            .by_command
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, &i)| i)
            .collect();
        inside.sort_unstable();
        (state.oo.by_ns.get(fqn).copied(), inside)
    };
    // The objects whose commands the namespace holds go with it.
    for id in inside {
        let _ = destroy_object(interp, vm, id);
    }
    match own {
        Some(id) => {
            let _ = destroy_object(interp, vm, id);
            true
        }
        None => false,
    }
}
